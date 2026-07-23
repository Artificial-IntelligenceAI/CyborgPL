use std::collections::HashMap;
use std::path::Path;

use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::targets::{CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine};
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum, FloatType, StructType};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FloatValue, FunctionValue, PointerValue};
use inkwell::{AddressSpace, FloatPredicate, IntPredicate, OptimizationLevel};

use crate::ast::*;

/// The GMP shim functions (runtime/gmp/bignum_shim.c) backing `bignum`.
struct BignumFns<'ctx> {
    new: FunctionValue<'ctx>,
    set_d: FunctionValue<'ctx>,
    set_str: FunctionValue<'ctx>,
    copy: FunctionValue<'ctx>,
    add: FunctionValue<'ctx>,
    sub: FunctionValue<'ctx>,
    mul: FunctionValue<'ctx>,
    div: FunctionValue<'ctx>,
    to_string: FunctionValue<'ctx>,
    free: FunctionValue<'ctx>,
}

/// One entry per variable declared directly in a block, remembering
/// whatever needs to happen to `Codegen::variables` when that block ends:
/// either the key simply disappears (nothing of that name existed before
/// this block), or an outer variable of the same (name, type) was
/// shadowed and must reappear.
enum ScopeEntry<'ctx> {
    New((String, Type)),
    Shadowed((String, Type), (PointerValue<'ctx>, BasicTypeEnum<'ctx>)),
}

impl<'ctx> ScopeEntry<'ctx> {
    fn key(&self) -> &(String, Type) {
        match self {
            ScopeEntry::New(k) => k,
            ScopeEntry::Shadowed(k, _) => k,
        }
    }
}

pub struct Codegen<'ctx> {
    context: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    functions: HashMap<String, FunctionValue<'ctx>>,
    /// Keyed by (name, type) rather than just name, so a name can be shared
    /// by variables of different types -- ref:var:TYPE 'name' picks between
    /// them by type at each reference site.
    variables: HashMap<(String, Type), (PointerValue<'ctx>, BasicTypeEnum<'ctx>)>,
    /// Stack of block scopes, innermost last. Each `compile_block` call
    /// pushes one frame and pops it when the block ends, restoring
    /// whatever `variables` looked like before that block ran.
    scopes: Vec<Vec<ScopeEntry<'ctx>>>,
    /// Bignum handles allocated as *intermediate* expression results (only
    /// source today: the binary-op arm below) during the statement
    /// currently being compiled -- never a named variable's own handle.
    /// Drained and freed once that statement finishes, since nothing else
    /// will ever reference them (every consumer -- coerce_to_bignum,
    /// bignum_to_string, an enclosing binary op -- reads/copies, never
    /// adopts the pointer). Each entry keeps both the raw handle (to free)
    /// and the exact wrapped value produced for it (`build_extract_value`
    /// makes a *new* instruction every time it's called, even reading the
    /// same field back out of the same struct -- so identifying "is this
    /// literally the value this statement is returning" has to compare
    /// against that original produced value, not a freshly re-extracted
    /// pointer, or the comparison never matches).
    bignum_temps: Vec<(PointerValue<'ctx>, BasicValueEnum<'ctx>)>,
    printf_fn: FunctionValue<'ctx>,
    /// libm's `pow`, backing both `xx` (power) and `xxx` (tetration).
    pow_fn: FunctionValue<'ctx>,
    bignum: BignumFns<'ctx>,
}

impl<'ctx> Codegen<'ctx> {
    pub fn new(context: &'ctx Context, module_name: &str) -> Self {
        let module = context.create_module(module_name);
        let builder = context.create_builder();

        // These are all implemented on top of libc/libm, linked in via `cc`.
        let i8_ptr = context.ptr_type(AddressSpace::default());
        let f64_type = context.f64_type();

        let printf_type = context.i32_type().fn_type(&[i8_ptr.into()], true);
        let printf_fn = module.add_function("printf", printf_type, Some(Linkage::External));

        let pow_type = f64_type.fn_type(&[f64_type.into(), f64_type.into()], false);
        let pow_fn = module.add_function("pow", pow_type, Some(Linkage::External));

        // runtime/gmp/bignum_shim.c -- every handle is an opaque i8_ptr, so
        // these signatures are the same shape regardless of what GMP itself
        // actually does under the hood.
        let void_ty = context.void_type();
        let i64_ty = context.i64_type();
        let bignum = BignumFns {
            new: module.add_function("bignum_new", i8_ptr.fn_type(&[i64_ty.into()], false), Some(Linkage::External)),
            set_d: module.add_function(
                "bignum_set_d",
                void_ty.fn_type(&[i8_ptr.into(), f64_type.into()], false),
                Some(Linkage::External),
            ),
            set_str: module.add_function(
                "bignum_set_str",
                void_ty.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
                Some(Linkage::External),
            ),
            copy: module.add_function(
                "bignum_copy",
                void_ty.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
                Some(Linkage::External),
            ),
            add: module.add_function(
                "bignum_add",
                void_ty.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
                Some(Linkage::External),
            ),
            sub: module.add_function(
                "bignum_sub",
                void_ty.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
                Some(Linkage::External),
            ),
            mul: module.add_function(
                "bignum_mul",
                void_ty.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
                Some(Linkage::External),
            ),
            div: module.add_function(
                "bignum_div",
                void_ty.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
                Some(Linkage::External),
            ),
            to_string: module.add_function(
                "bignum_to_string",
                i8_ptr.fn_type(&[i8_ptr.into()], false),
                Some(Linkage::External),
            ),
            free: module.add_function(
                "bignum_free",
                void_ty.fn_type(&[i8_ptr.into()], false),
                Some(Linkage::External),
            ),
        };

        Codegen {
            context,
            module,
            builder,
            functions: HashMap::new(),
            variables: HashMap::new(),
            scopes: Vec::new(),
            bignum_temps: Vec::new(),
            printf_fn,
            pow_fn,
            bignum,
        }
    }

    pub fn module(&self) -> &Module<'ctx> {
        &self.module
    }

    /// Lower the LLVM IR built up so far into a native `.o` object file,
    /// targeting whatever machine this compiler itself is running on.
    pub fn write_object_file(&self, path: &Path) -> Result<(), String> {
        Target::initialize_native(&InitializationConfig::default())?;

        let triple = TargetMachine::get_default_triple();
        let target = Target::from_triple(&triple).map_err(|e| e.to_string())?;
        let cpu = TargetMachine::get_host_cpu_name().to_string();
        let features = TargetMachine::get_host_cpu_features().to_string();

        let target_machine = target
            .create_target_machine(
                &triple,
                &cpu,
                &features,
                OptimizationLevel::Default,
                RelocMode::PIC,
                CodeModel::Default,
            )
            .ok_or("failed to create target machine for this host")?;

        target_machine
            .write_to_file(&self.module, FileType::Object, path)
            .map_err(|e| e.to_string())
    }

    fn basic_type(&self, ty: Type) -> BasicTypeEnum<'ctx> {
        match ty {
            Type::Num(width) => self.float_type_for(width).into(),
            Type::Bool => self.context.bool_type().into(),
            Type::Str => self.context.ptr_type(AddressSpace::default()).into(),
            Type::BigNum(_) => self.bignum_struct_type().into(),
            Type::Void => panic!("void has no runtime representation"),
        }
    }

    /// A `bignum` value is a pointer to a heap-allocated GMP handle
    /// (opaque to us -- see runtime/gmp/bignum_shim.c), but wrapped in a
    /// single-field struct rather than passed around as a bare pointer.
    /// This is deliberate: `str` is *also* a bare pointer, and compile_expr
    /// dispatches purely on the *shape* of a BasicValueEnum (FloatValue,
    /// PointerValue, etc.) with no separate type tag alongside it. Wrapping
    /// the pointer keeps `bignum` as its own distinct BasicValueEnum variant
    /// (StructValue), so it can never be silently confused with a `str`
    /// pointer at any of the existing dispatch points, without having to
    /// thread an explicit type alongside every value through codegen.
    fn bignum_struct_type(&self) -> StructType<'ctx> {
        self.context.struct_type(&[self.context.ptr_type(AddressSpace::default()).into()], false)
    }

    fn wrap_bignum_ptr(&self, ptr: PointerValue<'ctx>) -> BasicValueEnum<'ctx> {
        let undef = self.bignum_struct_type().get_undef();
        self.builder.build_insert_value(undef, ptr, 0, "bignum_wrap").unwrap().into_struct_value().into()
    }

    fn unwrap_bignum_ptr(&self, value: BasicValueEnum<'ctx>) -> PointerValue<'ctx> {
        self.builder
            .build_extract_value(value.into_struct_value(), 0, "bignum_ptr")
            .unwrap()
            .into_pointer_value()
    }

    /// Calls bignum_new(precision) and returns the resulting handle pointer
    /// (not yet wrapped -- callers combine this with wrap_bignum_ptr once
    /// the handle has been populated).
    fn bignum_new(&self, precision: u32) -> PointerValue<'ctx> {
        let prec = self.context.i64_type().const_int(precision as u64, false);
        self.builder
            .build_call(self.bignum.new, &[prec.into()], "bignum_new_call")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value()
    }

    /// Converts an already-compiled value into a *freshly allocated* bignum
    /// at the given precision -- always a fresh handle and a copy/convert,
    /// never just reusing an existing bignum's pointer directly, so that
    /// `var:bignum 'x' = ref:var:bignum 'y';` gives x and y independent
    /// values rather than aliasing the same underlying GMP handle (bignum
    /// is heap-backed, but has to behave *by value* at the language level,
    /// the same as every other type here).
    fn coerce_to_bignum(&self, value: BasicValueEnum<'ctx>, precision: u32) -> BasicValueEnum<'ctx> {
        let handle = self.bignum_new(precision);
        match value {
            BasicValueEnum::FloatValue(f) => {
                let as_f64 = self.coerce_float(f, 64);
                self.builder.build_call(self.bignum.set_d, &[handle.into(), as_f64.into()], "bignum_set_d_call").unwrap();
            }
            BasicValueEnum::PointerValue(p) => {
                self.builder.build_call(self.bignum.set_str, &[handle.into(), p.into()], "bignum_set_str_call").unwrap();
            }
            BasicValueEnum::StructValue(_) => {
                let src = self.unwrap_bignum_ptr(value);
                self.builder.build_call(self.bignum.copy, &[handle.into(), src.into()], "bignum_copy_call").unwrap();
            }
            other => panic!("cannot use {other:?} as a bignum value"),
        }
        self.wrap_bignum_ptr(handle)
    }

    fn float_type_for(&self, width: u32) -> FloatType<'ctx> {
        match width {
            16 => self.context.f16_type(),
            32 => self.context.f32_type(),
            64 => self.context.f64_type(),
            128 => self.context.f128_type(),
            other => panic!("unsupported num precision: {other} (the parser should have rejected this)"),
        }
    }

    fn float_bit_width(&self, ty: FloatType<'ctx>) -> u32 {
        if ty == self.context.f16_type() {
            16
        } else if ty == self.context.f32_type() {
            32
        } else if ty == self.context.f64_type() {
            64
        } else if ty == self.context.f128_type() {
            128
        } else {
            panic!("unrecognized float type in codegen")
        }
    }

    /// Widens or narrows a float to the given bit width, a no-op if it's
    /// already that width. Needed because two `num`s of different
    /// precisions can't be combined in an LLVM op directly (they must
    /// match), and libm's `pow` only accepts `double`.
    fn coerce_float(&self, f: FloatValue<'ctx>, target_width: u32) -> FloatValue<'ctx> {
        let current_width = self.float_bit_width(f.get_type());
        if current_width == target_width {
            return f;
        }
        let target_ty = self.float_type_for(target_width);
        if current_width < target_width {
            self.builder.build_float_ext(f, target_ty, "prec_ext").unwrap()
        } else {
            self.builder.build_float_trunc(f, target_ty, "prec_trunc").unwrap()
        }
    }

    /// If a value being stored/passed doesn't match the target num
    /// precision, converts it. No-op for bool/str.
    fn coerce_to_type(&self, value: BasicValueEnum<'ctx>, ty: Type) -> BasicValueEnum<'ctx> {
        match (value, ty) {
            (BasicValueEnum::FloatValue(f), Type::Num(width)) => self.coerce_float(f, width).into(),
            (_, Type::BigNum(precision)) => self.coerce_to_bignum(value, precision),
            _ => value,
        }
    }

    /// Widens the narrower of two floats to match the wider one, so binary
    /// ops always see matching operand types. No-op if they already match
    /// or aren't both floats.
    fn match_float_widths(
        &self,
        l: BasicValueEnum<'ctx>,
        r: BasicValueEnum<'ctx>,
    ) -> (BasicValueEnum<'ctx>, BasicValueEnum<'ctx>) {
        if let (BasicValueEnum::FloatValue(lf), BasicValueEnum::FloatValue(rf)) = (l, r) {
            if lf.get_type() != rf.get_type() {
                let lw = self.float_bit_width(lf.get_type());
                let rw = self.float_bit_width(rf.get_type());
                return if lw < rw {
                    (self.coerce_float(lf, rw).into(), r)
                } else {
                    (l, self.coerce_float(rf, lw).into())
                };
            }
        }
        (l, r)
    }

    /// Records that `key` was just declared in the *current* (innermost)
    /// block, so its binding can be undone when that block ends. A no-op
    /// if the current block already owns this key (redeclaring the same
    /// name/type twice in one block is just a rebind, not new shadowing --
    /// the original scope entry already remembers what to restore).
    fn declare_scoped(&mut self, key: (String, Type)) {
        let frame = self.scopes.last().expect("declare_scoped called outside any block");
        if frame.iter().any(|e| e.key() == &key) {
            return;
        }
        let entry = match self.variables.get(&key) {
            Some(&old) => ScopeEntry::Shadowed(key, old),
            None => ScopeEntry::New(key),
        };
        self.scopes.last_mut().unwrap().push(entry);
    }

    /// Frees the bignum handle currently stored in `key`'s variable slot.
    /// Only valid to call while that slot's alloca is still live.
    fn free_bignum_var(&mut self, key: &(String, Type)) {
        let (ptr, llvm_ty) = *self.variables.get(key).expect("free_bignum_var on unknown variable");
        let loaded = self.builder.build_load(llvm_ty, ptr, "bignum_for_free").unwrap();
        let handle = self.unwrap_bignum_ptr(loaded);
        self.free_bignum_ptr(handle);
    }

    fn free_bignum_ptr(&mut self, ptr: PointerValue<'ctx>) {
        self.builder.build_call(self.bignum.free, &[ptr.into()], "bignum_free_call").unwrap();
    }

    /// Ends the innermost block scope: every bignum it owns is freed (skip
    /// this only when the block already ended in `return`, since Return
    /// frees everything itself before the terminator -- freeing again
    /// here would double-free), then each entry's binding is undone
    /// (removed if `New`, restored to the outer value if `Shadowed`).
    /// The `variables` bookkeeping always happens regardless of
    /// termination -- it reflects lexical structure, not control flow, and
    /// later sibling code needs it to be correct either way.
    fn pop_scope(&mut self, emit_frees: bool) {
        let entries = self.scopes.pop().expect("pop_scope with no open scope");
        for entry in entries.into_iter().rev() {
            if emit_frees {
                if let (_, Type::BigNum(_)) = entry.key() {
                    self.free_bignum_var(entry.key());
                }
            }
            match entry {
                ScopeEntry::New(key) => {
                    self.variables.remove(&key);
                }
                ScopeEntry::Shadowed(key, old) => {
                    self.variables.insert(key, old);
                }
            }
        }
    }

    pub fn compile_program(&mut self, program: &Program) -> Result<(), String> {
        // Declare every function signature up front so calls to functions
        // defined later in the file (or mutually recursive calls) resolve.
        for f in &program.functions {
            self.declare_function(f);
        }
        for f in &program.functions {
            self.compile_function(f)?;
        }
        self.compile_entry(&program.entry)
    }

    fn declare_function(&mut self, f: &Function) {
        let param_types: Vec<BasicMetadataTypeEnum> =
            f.params.iter().map(|p| self.basic_type(p.ty).into()).collect();

        let fn_type = match f.return_type {
            Type::Num(width) => self.float_type_for(width).fn_type(&param_types, false),
            Type::Bool => self.context.bool_type().fn_type(&param_types, false),
            Type::Str => self.context.ptr_type(AddressSpace::default()).fn_type(&param_types, false),
            Type::BigNum(_) => self.bignum_struct_type().fn_type(&param_types, false),
            Type::Void => self.context.void_type().fn_type(&param_types, false),
        };

        let function = self.module.add_function(&f.name, fn_type, None);
        self.functions.insert(f.name.clone(), function);
    }

    /// Compiles the `START...END` block into the actual `main` the C runtime
    /// calls to start the process — this is the language's real entry point.
    fn compile_entry(&mut self, entry: &Block) -> Result<(), String> {
        let fn_type = self.context.i32_type().fn_type(&[], false);
        let function = self.module.add_function("main", fn_type, None);
        let block = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(block);
        self.variables.clear();
        self.scopes.clear();

        self.compile_block(entry)?;

        let current_block = self.builder.get_insert_block().unwrap();
        if current_block.get_terminator().is_none() {
            let zero = self.context.i32_type().const_int(0, false);
            self.builder.build_return(Some(&zero)).unwrap();
        }
        Ok(())
    }

    fn compile_function(&mut self, f: &Function) -> Result<(), String> {
        let function = self.functions[&f.name];
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);
        self.variables.clear();
        self.scopes.clear();

        for (i, param) in f.params.iter().enumerate() {
            let value = function.get_nth_param(i as u32).unwrap();
            let ty = self.basic_type(param.ty);
            let alloca = self.builder.build_alloca(ty, &param.name).unwrap();
            self.builder.build_store(alloca, value).unwrap();
            self.variables.insert((param.name.clone(), param.ty), (alloca, ty));
        }

        self.compile_block(&f.body)?;

        // Every LLVM basic block must end in a terminator. If the source
        // fell off the end of the function without an explicit `return`,
        // patch one in (a real type checker would flag this as missing
        // a return on some path instead of silently defaulting).
        let current_block = self.builder.get_insert_block().unwrap();
        if current_block.get_terminator().is_none() {
            match f.return_type {
                Type::Void => {
                    self.builder.build_return(None).unwrap();
                }
                Type::Num(width) => {
                    let zero = self.float_type_for(width).const_float(0.0);
                    self.builder.build_return(Some(&zero)).unwrap();
                }
                Type::Bool => {
                    let zero = self.context.bool_type().const_int(0, false);
                    self.builder.build_return(Some(&zero)).unwrap();
                }
                Type::Str => {
                    let null = self.context.ptr_type(AddressSpace::default()).const_null();
                    self.builder.build_return(Some(&null)).unwrap();
                }
                Type::BigNum(_) => {
                    let null_ptr = self.context.ptr_type(AddressSpace::default()).const_null();
                    let zero = self.wrap_bignum_ptr(null_ptr);
                    self.builder.build_return(Some(&zero)).unwrap();
                }
            }
        }
        Ok(())
    }

    fn compile_block(&mut self, block: &Block) -> Result<(), String> {
        self.scopes.push(Vec::new());
        for stmt in block {
            // Once a block is terminated (e.g. by `return`), any further
            // statements are unreachable; don't try to emit code for them.
            if self.builder.get_insert_block().unwrap().get_terminator().is_some() {
                break;
            }
            self.compile_stmt(stmt)?;
        }
        // If a `return` inside this block already terminated it, it also
        // already freed every bignum in every open scope itself (see
        // Stmt::Return) -- emit_frees=false here avoids freeing them again.
        let terminated = self.builder.get_insert_block().unwrap().get_terminator().is_some();
        self.pop_scope(!terminated);
        Ok(())
    }

    fn compile_stmt(&mut self, stmt: &Stmt) -> Result<(), String> {
        match stmt {
            Stmt::VarDecl(name, ty, expr) => {
                let value = self.compile_expr(expr)?;
                let value = self.coerce_to_type(value, *ty);
                let key = (name.clone(), *ty);

                // Re-declaring a bignum name that's already alive (same
                // block, or shadowing an outer one) would otherwise leak
                // its old handle -- free it before the slot's replaced.
                if let (Type::BigNum(_), Some(_)) = (*ty, self.variables.get(&key)) {
                    self.free_bignum_var(&key);
                }

                let llvm_ty = self.basic_type(*ty);
                let alloca = self.builder.build_alloca(llvm_ty, name).unwrap();
                self.builder.build_store(alloca, value).unwrap();
                self.declare_scoped(key.clone());
                self.variables.insert(key, (alloca, llvm_ty));
            }
            Stmt::Assign(name, ty, expr) => {
                let value = self.compile_expr(expr)?;
                let value = self.coerce_to_type(value, *ty);
                let key = (name.clone(), *ty);
                let (ptr, _ty) = *self
                    .variables
                    .get(&key)
                    .ok_or_else(|| format!("undefined variable '{name}' of type {ty:?}"))?;
                // Reassignment always stores a fresh handle (coerce_to_type
                // -> coerce_to_bignum), so the old one must be freed here
                // or it leaks -- the slot itself doesn't change, only what
                // it points at.
                if let Type::BigNum(_) = *ty {
                    self.free_bignum_var(&key);
                }
                self.builder.build_store(ptr, value).unwrap();
            }
            Stmt::Return(expr) => {
                // A bare `return ref:var:bignum 'x';` hands out 'x''s own
                // handle (reading a variable doesn't copy it -- only
                // assignment does), so that one specific variable must
                // survive the free pass below or the caller gets a
                // dangling pointer. Anything else (a computed bignum
                // expression, a different type entirely) doesn't alias any
                // local, since bignum binary ops always allocate a fresh
                // destination handle.
                let skip_key = match expr {
                    Some(Expr::Var(n, ty @ Type::BigNum(_))) => Some((n.clone(), *ty)),
                    _ => None,
                };
                let value = match expr {
                    Some(e) => Some(self.compile_expr(e)?),
                    None => None,
                };
                // If the returned value is itself a fresh bignum temporary
                // (`return a + b;`), it must survive the temp-draining pass
                // below -- it's the function's actual return value now, not
                // a discarded intermediate. Compared against the exact
                // value `compile_expr` produced for it, not a freshly
                // re-extracted pointer (see the `bignum_temps` field docs).
                let temps: Vec<(PointerValue<'ctx>, BasicValueEnum<'ctx>)> = self.bignum_temps.drain(..).collect();
                for (ptr, produced) in temps {
                    let is_returned = matches!(value, Some(v) if v == produced);
                    if !is_returned {
                        self.free_bignum_ptr(ptr);
                    }
                }
                // A `return` exits every block it's nested in at once, so
                // every currently-open scope must be freed here -- not
                // just the innermost. This only frees; it deliberately
                // doesn't pop `self.scopes` (that stays for the enclosing
                // compile_block calls to unwind normally once control
                // returns to them, since sibling/later code compiled after
                // this dead end still needs correct scoping).
                let to_free: Vec<(String, Type)> = self
                    .scopes
                    .iter()
                    .rev()
                    .flat_map(|frame| frame.iter().rev())
                    .map(ScopeEntry::key)
                    .filter(|key| matches!(key.1, Type::BigNum(_)) && Some(*key) != skip_key.as_ref())
                    .cloned()
                    .collect();
                for key in &to_free {
                    self.free_bignum_var(key);
                }
                match value {
                    Some(value) => {
                        self.builder.build_return(Some(&value)).unwrap();
                    }
                    None => {
                        self.builder.build_return(None).unwrap();
                    }
                };
            }
            Stmt::Print(segments) => {
                let mut fmt = String::new();
                let mut args: Vec<BasicMetadataValueEnum> = Vec::new();
                for seg in segments {
                    match seg {
                        // Literal text is inserted as-is, except any '%' it
                        // contains must be escaped so printf's own format
                        // parser doesn't mistake it for a specifier.
                        PrintSegment::Str(s) => fmt.push_str(&s.replace('%', "%%")),
                        PrintSegment::Expr(e) => {
                            let value = self.compile_expr(e)?;
                            let (frag, arg) = self.value_fmt(value);
                            fmt.push_str(frag);
                            args.push(arg);
                        }
                    }
                }
                fmt.push('\n');

                let fmt_global = self.builder.build_global_string_ptr(&fmt, "fmt").unwrap();
                let mut call_args: Vec<BasicMetadataValueEnum> = vec![fmt_global.as_pointer_value().into()];
                call_args.extend(args);
                self.builder.build_call(self.printf_fn, &call_args, "printf_call").unwrap();
            }
            Stmt::ExprStmt(expr) => {
                // Not compile_expr(expr): a call to a void function (the
                // common case here -- e.g. a function that just prints) has
                // no return value to unwrap, and compile_expr's Expr::Call
                // arm assumes every call is used in value position and
                // panics otherwise. A bare statement never needs the value.
                if let Expr::Call(name, args) = expr {
                    let function = *self
                        .functions
                        .get(name)
                        .ok_or_else(|| format!("undefined function '{name}'"))?;
                    let arg_values: Vec<BasicMetadataValueEnum> = args
                        .iter()
                        .map(|a| self.compile_expr(a).map(Into::into))
                        .collect::<Result<_, _>>()?;
                    self.builder.build_call(function, &arg_values, "call").unwrap();
                } else {
                    self.compile_expr(expr)?;
                }
            }
            Stmt::If(cond, then_block, else_block) => {
                let function = self.current_function();
                let cond_value = self.compile_expr(cond)?.into_int_value();

                let then_bb = self.context.append_basic_block(function, "then");
                let else_bb = self.context.append_basic_block(function, "else");
                let merge_bb = self.context.append_basic_block(function, "merge");

                self.builder
                    .build_conditional_branch(cond_value, then_bb, else_bb)
                    .unwrap();

                self.builder.position_at_end(then_bb);
                self.compile_block(then_block)?;
                if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
                    self.builder.build_unconditional_branch(merge_bb).unwrap();
                }

                self.builder.position_at_end(else_bb);
                if let Some(else_stmts) = else_block {
                    self.compile_block(else_stmts)?;
                }
                if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
                    self.builder.build_unconditional_branch(merge_bb).unwrap();
                }

                self.builder.position_at_end(merge_bb);
            }
            Stmt::While(cond, body) => {
                let function = self.current_function();
                let cond_bb = self.context.append_basic_block(function, "while_cond");
                let body_bb = self.context.append_basic_block(function, "while_body");
                let merge_bb = self.context.append_basic_block(function, "while_end");

                self.builder.build_unconditional_branch(cond_bb).unwrap();

                self.builder.position_at_end(cond_bb);
                let cond_value = self.compile_expr(cond)?.into_int_value();
                self.builder
                    .build_conditional_branch(cond_value, body_bb, merge_bb)
                    .unwrap();

                self.builder.position_at_end(body_bb);
                self.compile_block(body)?;
                if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
                    self.builder.build_unconditional_branch(cond_bb).unwrap();
                }

                self.builder.position_at_end(merge_bb);
            }
        }
        // Free any bignum temporaries created while evaluating this
        // statement's own expression(s) (Stmt::Return drains and frees its
        // own -- protecting the value it returns -- before building its
        // terminator, so this is empty by the time we get here for that
        // case). Skipped if the block already ended in `return`: there's
        // no valid insertion point left before a terminator, but there's
        // also nothing left to free -- Return already handled it.
        if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
            let temps: Vec<(PointerValue<'ctx>, BasicValueEnum<'ctx>)> = self.bignum_temps.drain(..).collect();
            for (ptr, _) in temps {
                self.free_bignum_ptr(ptr);
            }
        } else {
            self.bignum_temps.clear();
        }
        Ok(())
    }

    fn compile_expr(&mut self, expr: &Expr) -> Result<BasicValueEnum<'ctx>, String> {
        Ok(match expr {
            Expr::Num(n) => self.context.f64_type().const_float(*n).into(),
            Expr::Bool(b) => self.context.bool_type().const_int(*b as u64, false).into(),
            Expr::Str(s) => self.builder.build_global_string_ptr(s, "str").unwrap().as_pointer_value().into(),
            Expr::Var(name, ty) => {
                let (ptr, llvm_ty) = *self
                    .variables
                    .get(&(name.clone(), *ty))
                    .ok_or_else(|| format!("undefined variable '{name}' of type {ty:?}"))?;
                self.builder.build_load(llvm_ty, ptr, name).unwrap()
            }
            Expr::Unary(op, inner) => {
                let value = self.compile_expr(inner)?;
                match (op, value) {
                    (UnOp::Neg, BasicValueEnum::FloatValue(f)) => {
                        self.builder.build_float_neg(f, "neg").unwrap().into()
                    }
                    (UnOp::Not, BasicValueEnum::IntValue(i)) => {
                        self.builder.build_not(i, "not").unwrap().into()
                    }
                    (op, other) => panic!("unary {op:?} not supported on {other:?}"),
                }
            }
            Expr::Binary(lhs, op, rhs) => {
                let l = self.compile_expr(lhs)?;
                let r = self.compile_expr(rhs)?;
                // Two nums of different precisions can't be combined by an
                // LLVM op directly -- widen the narrower one to match first.
                let (l, r) = self.match_float_widths(l, r);

                match (l, r) {
                    (BasicValueEnum::FloatValue(lf), BasicValueEnum::FloatValue(rf)) => match op {
                        BinOp::Add => self.builder.build_float_add(lf, rf, "add").unwrap().into(),
                        BinOp::Sub => self.builder.build_float_sub(lf, rf, "sub").unwrap().into(),
                        BinOp::Mul => self.builder.build_float_mul(lf, rf, "mul").unwrap().into(),
                        BinOp::Div => self.builder.build_float_div(lf, rf, "div").unwrap().into(),
                        BinOp::Pow => {
                            // libm's pow is fixed to `double` regardless of num's precision.
                            let lf64 = self.coerce_float(lf, 64);
                            let rf64 = self.coerce_float(rf, 64);
                            self.builder
                                .build_call(self.pow_fn, &[lf64.into(), rf64.into()], "pow")
                                .unwrap()
                                .try_as_basic_value()
                                .basic()
                                .unwrap()
                        }
                        BinOp::Tetration => {
                            let lf64 = self.coerce_float(lf, 64);
                            let rf64 = self.coerce_float(rf, 64);
                            self.compile_tetration(lf64, rf64).into()
                        }
                        BinOp::Eq => self
                            .builder
                            .build_float_compare(FloatPredicate::OEQ, lf, rf, "eq")
                            .unwrap()
                            .into(),
                        BinOp::Ne => self
                            .builder
                            .build_float_compare(FloatPredicate::ONE, lf, rf, "ne")
                            .unwrap()
                            .into(),
                        BinOp::Lt => self
                            .builder
                            .build_float_compare(FloatPredicate::OLT, lf, rf, "lt")
                            .unwrap()
                            .into(),
                        BinOp::Gt => self
                            .builder
                            .build_float_compare(FloatPredicate::OGT, lf, rf, "gt")
                            .unwrap()
                            .into(),
                        BinOp::Le => self
                            .builder
                            .build_float_compare(FloatPredicate::OLE, lf, rf, "le")
                            .unwrap()
                            .into(),
                        BinOp::Ge => self
                            .builder
                            .build_float_compare(FloatPredicate::OGE, lf, rf, "ge")
                            .unwrap()
                            .into(),
                        BinOp::And | BinOp::Or => panic!("{op:?} requires bool operands, not num"),
                    },
                    (BasicValueEnum::IntValue(li), BasicValueEnum::IntValue(ri)) => match op {
                        BinOp::Eq => self.builder.build_int_compare(IntPredicate::EQ, li, ri, "eq").unwrap().into(),
                        BinOp::Ne => self.builder.build_int_compare(IntPredicate::NE, li, ri, "ne").unwrap().into(),
                        BinOp::And => self.builder.build_and(li, ri, "and").unwrap().into(),
                        BinOp::Or => self.builder.build_or(li, ri, "or").unwrap().into(),
                        _ => panic!("{op:?} not supported on bool operands"),
                    },
                    // Simplification, stated plainly rather than hidden: the
                    // destination of an intermediate bignum operation always
                    // uses the default precision, regardless of the
                    // operands' own (possibly custom) precisions -- unlike
                    // num, this doesn't "widen to the larger operand".
                    // Assigning the result into an explicitly precise
                    // variable still works (coerce_to_bignum copies into a
                    // fresh handle at *that* variable's declared precision),
                    // but an intermediate expression used elsewhere won't
                    // retroactively regain precision this step didn't have.
                    (BasicValueEnum::StructValue(_), BasicValueEnum::StructValue(_)) => {
                        let shim_fn = match op {
                            BinOp::Add => self.bignum.add,
                            BinOp::Sub => self.bignum.sub,
                            BinOp::Mul => self.bignum.mul,
                            BinOp::Div => self.bignum.div,
                            _ => panic!("{op:?} not supported on bignum yet"),
                        };
                        let lp = self.unwrap_bignum_ptr(l);
                        let rp = self.unwrap_bignum_ptr(r);
                        let dst = self.bignum_new(DEFAULT_BIGNUM_PRECISION);
                        self.builder.build_call(shim_fn, &[dst.into(), lp.into(), rp.into()], "bignum_op_call").unwrap();
                        // Nothing else ever adopts this handle -- whatever
                        // consumes it (a store via coerce_to_bignum, a print,
                        // an enclosing binary op) only reads or copies from
                        // it. Registered here so the end of whichever
                        // statement this expression is part of can free it.
                        let wrapped = self.wrap_bignum_ptr(dst);
                        self.bignum_temps.push((dst, wrapped));
                        wrapped
                    }
                    (l, r) => panic!("binary {op:?} used with mismatched operand types {l:?} / {r:?}"),
                }
            }
            Expr::Call(name, args) => {
                let function = *self
                    .functions
                    .get(name)
                    .ok_or_else(|| format!("undefined function '{name}'"))?;
                let arg_values: Vec<BasicMetadataValueEnum> = args
                    .iter()
                    .map(|a| self.compile_expr(a).map(Into::into))
                    .collect::<Result<_, _>>()?;
                let call = self.builder.build_call(function, &arg_values, "call").unwrap();
                call.try_as_basic_value()
                    .basic()
                    .expect("function used in expression position must return a value")
            }
        })
    }

    /// The printf-style format fragment (no surrounding text) and matching
    /// call argument for a compiled value. Used to build print's combined
    /// format string across all of its segments.
    fn value_fmt(&self, value: BasicValueEnum<'ctx>) -> (&'static str, BasicMetadataValueEnum<'ctx>) {
        match value {
            // printf's varargs ABI expects `double` regardless of num's
            // declared precision (C's default argument promotion, which we
            // have to do explicitly since LLVM won't do it for us).
            BasicValueEnum::FloatValue(f) => ("%g", self.coerce_float(f, 64).into()),
            BasicValueEnum::PointerValue(p) => ("%s", p.into()),
            BasicValueEnum::IntValue(i) => {
                // Only bools (i1) reach here. The actual value is only known
                // at runtime (it could come from a comparison, a variable,
                // anything), so picking "true" vs "false" text needs a
                // runtime select between the two string constants, not a
                // compile-time choice.
                let true_str = self.builder.build_global_string_ptr("true", "true_str").unwrap();
                let false_str = self.builder.build_global_string_ptr("false", "false_str").unwrap();
                let chosen = self
                    .builder
                    .build_select(i, true_str.as_pointer_value(), false_str.as_pointer_value(), "bool_str")
                    .unwrap();
                ("%s", chosen.into())
            }
            BasicValueEnum::StructValue(_) => {
                let ptr = self.unwrap_bignum_ptr(value);
                let str_ptr = self
                    .builder
                    .build_call(self.bignum.to_string, &[ptr.into()], "bignum_to_string_call")
                    .unwrap()
                    .try_as_basic_value()
                    .basic()
                    .unwrap();
                ("%s", str_ptr.into())
            }
            other => panic!("unsupported value for text formatting: {other:?}"),
        }
    }

    /// `xxx`: a xxx b = a ^ (a ^ (a ^ ... )) with `b` copies of `a`. `b` is
    /// only known at runtime, so this is an actual loop (mirroring how
    /// `while` is compiled), not a fixed chain of multiplications.
    fn compile_tetration(&mut self, base: FloatValue<'ctx>, height: FloatValue<'ctx>) -> FloatValue<'ctx> {
        let function = self.current_function();
        let i64_ty = self.context.i64_type();
        let f64_ty = self.context.f64_type();

        let height_int = self.builder.build_float_to_signed_int(height, i64_ty, "tet_height").unwrap();

        let result_slot = self.builder.build_alloca(f64_ty, "tet_result").unwrap();
        self.builder.build_store(result_slot, base).unwrap();
        let counter_slot = self.builder.build_alloca(i64_ty, "tet_i").unwrap();
        self.builder.build_store(counter_slot, i64_ty.const_int(2, true)).unwrap();

        let cond_bb = self.context.append_basic_block(function, "tet_cond");
        let body_bb = self.context.append_basic_block(function, "tet_body");
        let end_bb = self.context.append_basic_block(function, "tet_end");
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        // height copies of `a` means (height - 1) more pow() calls after the
        // starting value of `a`, so the counter runs from 2 up to `height`.
        self.builder.position_at_end(cond_bb);
        let counter = self.builder.build_load(i64_ty, counter_slot, "tet_i_load").unwrap().into_int_value();
        let keep_going = self
            .builder
            .build_int_compare(IntPredicate::SLE, counter, height_int, "tet_test")
            .unwrap();
        self.builder.build_conditional_branch(keep_going, body_bb, end_bb).unwrap();

        self.builder.position_at_end(body_bb);
        let current = self.builder.build_load(f64_ty, result_slot, "tet_result_load").unwrap().into_float_value();
        let next = self
            .builder
            .build_call(self.pow_fn, &[base.into(), current.into()], "tet_pow")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_float_value();
        self.builder.build_store(result_slot, next).unwrap();
        let counter_next = self.builder.build_int_add(counter, i64_ty.const_int(1, true), "tet_i_next").unwrap();
        self.builder.build_store(counter_slot, counter_next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(end_bb);
        self.builder.build_load(f64_ty, result_slot, "tet_final").unwrap().into_float_value()
    }

    fn current_function(&self) -> FunctionValue<'ctx> {
        self.builder
            .get_insert_block()
            .unwrap()
            .get_parent()
            .unwrap()
    }
}
