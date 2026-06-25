//! Codegen the MIR to the LLVM IR.
//!
//! Hopefully useful general knowledge about codegen:
//!
//! * There's no way to find out the [`Ty`] type of a [`Value`]. Doing so
//!   would be "trying to get the eggs out of an omelette" (credit:
//!   pcwalton). You can, instead, find out its [`llvm::Type`] by calling [`val_ty`],
//!   but one [`llvm::Type`] corresponds to many [`Ty`]s; for instance, `tup(int, int,
//!   int)` and `rec(x=int, y=int, z=int)` will have the same [`llvm::Type`].
//!
//! [`Ty`]: rustc_middle::ty::Ty
//! [`val_ty`]: crate::common::val_ty

use std::time::Instant;

use rustc_codegen_ssa::ModuleCodegen;
use rustc_codegen_ssa::base::maybe_create_entry_wrapper;
use rustc_codegen_ssa::mono_item::MonoItemExt;
use rustc_codegen_ssa::traits::*;
use rustc_data_structures::small_c_str::SmallCStr;
use rustc_hir::attrs::Linkage;
use rustc_middle::dep_graph;
use rustc_middle::middle::codegen_fn_attrs::{CodegenFnAttrs, SanitizerFnAttrs};
use rustc_middle::mono::Visibility;
use rustc_middle::ty::TyCtxt;
use rustc_session::config::{DebugInfo, Offload};
use rustc_span::Symbol;
use rustc_target::spec::SanitizerSet;

use super::ModuleLlvm;
use crate::attributes;
use crate::builder::Builder;
use crate::builder::gpu_offload::OffloadGlobals;
use crate::context::CodegenCx;
use crate::llvm::{self, Value};

pub(crate) struct ValueIter<'ll> {
    cur: Option<&'ll Value>,
    step: unsafe extern "C" fn(&'ll Value) -> Option<&'ll Value>,
}

impl<'ll> Iterator for ValueIter<'ll> {
    type Item = &'ll Value;

    fn next(&mut self) -> Option<&'ll Value> {
        let old = self.cur;
        if let Some(old) = old {
            self.cur = unsafe { (self.step)(old) };
        }
        old
    }
}

pub(crate) fn iter_globals(llmod: &llvm::Module) -> ValueIter<'_> {
    unsafe { ValueIter { cur: llvm::LLVMGetFirstGlobal(llmod), step: llvm::LLVMGetNextGlobal } }
}

pub(crate) fn compile_codegen_unit(
    tcx: TyCtxt<'_>,
    cgu_name: Symbol,
) -> (ModuleCodegen<ModuleLlvm>, u64) {
    let start_time = Instant::now();

    let dep_node = tcx.codegen_unit(cgu_name).codegen_dep_node(tcx);
    let (module, _) = tcx.dep_graph.with_task(
        dep_node,
        tcx,
        || module_codegen(tcx, cgu_name),
        Some(dep_graph::hash_result),
    );
    let time_to_codegen = start_time.elapsed();

    // We assume that the cost to run LLVM on a CGU is proportional to
    // the time we needed for codegenning it.
    let cost = time_to_codegen.as_nanos() as u64;

    fn module_codegen(tcx: TyCtxt<'_>, cgu_name: Symbol) -> ModuleCodegen<ModuleLlvm> {
        let cgu = tcx.codegen_unit(cgu_name);
        let _prof_timer =
            tcx.prof.generic_activity_with_arg_recorder("codegen_module", |recorder| {
                recorder.record_arg(cgu_name.to_string());
                recorder.record_arg(cgu.size_estimate().to_string());
            });
        // Instantiate monomorphizations without filling out definitions yet...
        let llvm_module = ModuleLlvm::new(tcx, cgu_name.as_str());
        {
            let mut cx = CodegenCx::new(tcx, cgu, &llvm_module);

            // Declare and store globals shared by all offload kernels
            //
            // These globals are left in the LLVM-IR host module so all kernels can access them.
            // They are necessary for correct offload execution. We do this here to simplify the
            // `offload` intrinsic, avoiding the need for tracking whether it's the first
            // intrinsic call or not.
            let has_host_offload = cx
                .sess()
                .opts
                .unstable_opts
                .offload
                .iter()
                .any(|o| matches!(o, Offload::Host(_) | Offload::Test));
            if has_host_offload && !cx.sess().target.is_like_gpu {
                cx.offload_globals.replace(Some(OffloadGlobals::declare(&cx)));
            }

            let mono_items = cx.codegen_unit.items_in_deterministic_order(cx.tcx);
            for &(mono_item, data) in &mono_items {
                mono_item.predefine::<Builder<'_, '_, '_>>(
                    &mut cx,
                    cgu_name.as_str(),
                    data.linkage,
                    data.visibility,
                );
            }

            // ... and now that we have everything pre-defined, fill out those definitions.
            for &(mono_item, item_data) in &mono_items {
                mono_item.define::<Builder<'_, '_, '_>>(&mut cx, cgu_name.as_str(), item_data);
            }

            // If this codegen unit contains the main function, also create the
            // wrapper here
            if let Some(entry) =
                maybe_create_entry_wrapper::<Builder<'_, '_, '_>>(&cx, cx.codegen_unit)
            {
                let attrs = attributes::sanitize_attrs(&cx, tcx, SanitizerFnAttrs::default());
                attributes::apply_to_llfn(entry, llvm::AttributePlace::Function, &attrs);
            }

            // Define Objective-C module info and module flags. Note, the module info will
            // also be added to the `llvm.compiler.used` variable, created later.
            //
            // These are only necessary when we need the linker to do its Objective-C-specific
            // magic. We could theoretically do it unconditionally, but at a slight cost to linker
            // performance in the common case where it's unnecessary.
            if !cx.objc_classrefs.borrow().is_empty() || !cx.objc_selrefs.borrow().is_empty() {
                if cx.objc_abi_version() == 1 {
                    cx.define_objc_module_info();
                }
                cx.add_objc_module_flags();
            }

            // Finalize code coverage by injecting the coverage map. Note, the coverage map will
            // also be added to the `llvm.compiler.used` variable, created next.
            if cx.sess().instrument_coverage() {
                cx.coverageinfo_finalize();
            }

            // Create the llvm.used variable.
            if !cx.used_statics.is_empty() {
                cx.create_used_variable_impl(c"llvm.used", &cx.used_statics);
            }

            // Create the llvm.compiler.used variable.
            {
                let compiler_used_statics = cx.compiler_used_statics.borrow();
                if !compiler_used_statics.is_empty() {
                    cx.create_used_variable_impl(c"llvm.compiler.used", &compiler_used_statics);
                }
            }

            // Run replace-all-uses-with for statics that need it. This must
            // happen after the llvm.used variables are created.
            for &(old_g, new_g) in cx.statics_to_rauw().borrow().iter() {
                unsafe {
                    llvm::LLVMReplaceAllUsesWith(old_g, new_g);
                    llvm::LLVMDeleteGlobal(old_g);
                }
            }

            // Finalize debuginfo
            if cx.sess().opts.debuginfo != DebugInfo::None {
                cx.debuginfo_finalize();
            }
        }

        // === clang! import injection ===
        // The `clang!` builtin macro records the C/C++ source paths into
        // `ResolverGlobalCtxt::clang_sources`. Here we compile each one via
        // clang to LLVM bitcode and link it straight into this codegen unit's
        // module, so the foreign declarations introduced by `clang!` resolve to
        // the real C/C++ definitions. We only inject once, into the first CGU,
        // to avoid duplicate symbol definitions across codegen units.
        let clang_sources = &tcx.resolutions(()).clang_sources;
        if !clang_sources.is_empty() {
            use std::sync::atomic::{AtomicBool, Ordering};
            static INJECTED: AtomicBool = AtomicBool::new(false);
            if !INJECTED.swap(true, Ordering::SeqCst) {
                let clang = std::env::var("CLANG").unwrap_or_else(|_| "clang".to_string());
                let linker = unsafe { llvm::LLVMRustLinkerNew(llvm_module.llmod()) };
                for (i, source) in clang_sources.iter().enumerate() {
                    let source = source.as_str();
                    let bc_path =
                        std::env::temp_dir().join(format!("rustc-clang-import-{i}.bc"));
                    let status = std::process::Command::new(&clang)
                        .args(["-emit-llvm", "-O1", "-c", source, "-o"])
                        .arg(&bc_path)
                        .status()
                        .unwrap_or_else(|e| panic!("clang!: failed to spawn `{clang}`: {e}"));
                    if !status.success() {
                        panic!("clang!: `{clang}` failed to compile {source}");
                    }
                    let data = std::fs::read(&bc_path).unwrap_or_else(|e| {
                        panic!("clang!: cannot read bitcode {}: {e}", bc_path.display())
                    });
                    let ok = unsafe {
                        llvm::LLVMRustLinkerAdd(
                            linker,
                            data.as_ptr() as *const libc::c_char,
                            data.len(),
                        )
                    };
                    if !ok {
                        unsafe { llvm::LLVMRustLinkerFree(linker) };
                        panic!("clang!: failed to link bitcode compiled from {source}");
                    }
                    eprintln!("[clang!] compiled and linked {source} into CGU `{cgu_name}`");
                }
                unsafe { llvm::LLVMRustLinkerFree(linker) };
            }
        }
        // === end clang! import injection ===

        ModuleCodegen::new_regular(cgu_name.to_string(), llvm_module)
    }

    (module, cost)
}

pub(crate) fn set_link_section(llval: &Value, attrs: &CodegenFnAttrs) {
    let Some(sect) = attrs.link_section else { return };
    let buf = SmallCStr::new(sect.as_str());
    llvm::set_section(llval, &buf);
}

pub(crate) fn linkage_to_llvm(linkage: Linkage) -> llvm::Linkage {
    match linkage {
        Linkage::External => llvm::Linkage::ExternalLinkage,
        Linkage::AvailableExternally => llvm::Linkage::AvailableExternallyLinkage,
        Linkage::LinkOnceAny => llvm::Linkage::LinkOnceAnyLinkage,
        Linkage::LinkOnceODR => llvm::Linkage::LinkOnceODRLinkage,
        Linkage::WeakAny => llvm::Linkage::WeakAnyLinkage,
        Linkage::WeakODR => llvm::Linkage::WeakODRLinkage,
        Linkage::Internal => llvm::Linkage::InternalLinkage,
        Linkage::ExternalWeak => llvm::Linkage::ExternalWeakLinkage,
        Linkage::Common => llvm::Linkage::CommonLinkage,
    }
}

pub(crate) fn visibility_to_llvm(linkage: Visibility) -> llvm::Visibility {
    match linkage {
        Visibility::Default => llvm::Visibility::Default,
        Visibility::Hidden => llvm::Visibility::Hidden,
        Visibility::Protected => llvm::Visibility::Protected,
    }
}

pub(crate) fn set_variable_sanitizer_attrs(llval: &Value, attrs: &CodegenFnAttrs) {
    if attrs.sanitizers.disabled.contains(SanitizerSet::ADDRESS)
        || attrs.sanitizers.disabled.contains(SanitizerSet::KERNELADDRESS)
    {
        unsafe { llvm::LLVMRustSetNoSanitizeAddress(llval) };
    }
    if attrs.sanitizers.disabled.contains(SanitizerSet::HWADDRESS)
        || attrs.sanitizers.disabled.contains(SanitizerSet::KERNELHWADDRESS)
    {
        unsafe { llvm::LLVMRustSetNoSanitizeHWAddress(llval) };
    }
}
