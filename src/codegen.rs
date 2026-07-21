use std::collections::HashMap;
use std::path::Path;

use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::targets::{CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine};
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FloatValue, FunctionValue, PointerValue};
use inkwell::{AddressSpace, FloatPredicate, IntPredicate, OptimizationLevel};

use crate::ast::*;

pub struct Codegen<'ctx> {
    context: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    functions: HashMap<String, FunctionValue<'ctx>>,
    variables: HashMap<String, (PointerValue<'ctx>, BasicTypeEnum<'ctx>)>,
    printf_fn: FunctionValue<'ctx>,
    /// libm's `pow`, backing both `xx` (power) and `xxx` (tetration).
    pow_fn: FunctionValue<'ctx>,
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

        Codegen {
            context,
            module,
            builder,
            functions: HashMap::new(),
            variables: HashMap::new(),
            printf_fn,
            pow_fn,
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
            Type::Num => self.context.f64_type().into(),
            Type::Bool => self.context.bool_type().into(),
            Type::Str => self.context.ptr_type(AddressSpace::default()).into(),
            Type::Void => panic!("void has no runtime representation"),
        }
    }

    pub fn compile_program(&mut self, program: &Program) {
        // Declare every function signature up front so calls to functions
        // defined later in the file (or mutually recursive calls) resolve.
        for f in &program.functions {
            self.declare_function(f);
        }
        for f in &program.functions {
            self.compile_function(f);
        }
        self.compile_entry(&program.entry);
    }

    fn declare_function(&mut self, f: &Function) {
        let param_types: Vec<BasicMetadataTypeEnum> =
            f.params.iter().map(|p| self.basic_type(p.ty).into()).collect();

        let fn_type = match f.return_type {
            Type::Num => self.context.f64_type().fn_type(&param_types, false),
            Type::Bool => self.context.bool_type().fn_type(&param_types, false),
            Type::Str => self.context.ptr_type(AddressSpace::default()).fn_type(&param_types, false),
            Type::Void => self.context.void_type().fn_type(&param_types, false),
        };

        let function = self.module.add_function(&f.name, fn_type, None);
        self.functions.insert(f.name.clone(), function);
    }

    /// Compiles the `START...END` block into the actual `main` the C runtime
    /// calls to start the process — this is the language's real entry point.
    fn compile_entry(&mut self, entry: &Block) {
        let fn_type = self.context.i32_type().fn_type(&[], false);
        let function = self.module.add_function("main", fn_type, None);
        let block = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(block);
        self.variables.clear();

        self.compile_block(entry);

        let current_block = self.builder.get_insert_block().unwrap();
        if current_block.get_terminator().is_none() {
            let zero = self.context.i32_type().const_int(0, false);
            self.builder.build_return(Some(&zero)).unwrap();
        }
    }

    fn compile_function(&mut self, f: &Function) {
        let function = self.functions[&f.name];
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);
        self.variables.clear();

        for (i, param) in f.params.iter().enumerate() {
            let value = function.get_nth_param(i as u32).unwrap();
            let ty = self.basic_type(param.ty);
            let alloca = self.builder.build_alloca(ty, &param.name).unwrap();
            self.builder.build_store(alloca, value).unwrap();
            self.variables.insert(param.name.clone(), (alloca, ty));
        }

        self.compile_block(&f.body);

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
                Type::Num => {
                    let zero = self.context.f64_type().const_float(0.0);
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
            }
        }
    }

    fn compile_block(&mut self, block: &Block) {
        for stmt in block {
            // Once a block is terminated (e.g. by `return`), any further
            // statements are unreachable; don't try to emit code for them.
            if self.builder.get_insert_block().unwrap().get_terminator().is_some() {
                break;
            }
            self.compile_stmt(stmt);
        }
    }

    fn compile_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::VarDecl(name, ty, expr) => {
                let value = self.compile_expr(expr);
                let llvm_ty = self.basic_type(*ty);
                let alloca = self.builder.build_alloca(llvm_ty, name).unwrap();
                self.builder.build_store(alloca, value).unwrap();
                self.variables.insert(name.clone(), (alloca, llvm_ty));
            }
            Stmt::Assign(name, expr) => {
                let value = self.compile_expr(expr);
                let (ptr, _ty) = self.variables[name];
                self.builder.build_store(ptr, value).unwrap();
            }
            Stmt::Return(expr) => {
                match expr {
                    Some(e) => {
                        let value = self.compile_expr(e);
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
                            let value = self.compile_expr(e);
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
                    let function = self.functions[name];
                    let arg_values: Vec<BasicMetadataValueEnum> =
                        args.iter().map(|a| self.compile_expr(a).into()).collect();
                    self.builder.build_call(function, &arg_values, "call").unwrap();
                } else {
                    self.compile_expr(expr);
                }
            }
            Stmt::If(cond, then_block, else_block) => {
                let function = self.current_function();
                let cond_value = self.compile_expr(cond).into_int_value();

                let then_bb = self.context.append_basic_block(function, "then");
                let else_bb = self.context.append_basic_block(function, "else");
                let merge_bb = self.context.append_basic_block(function, "merge");

                self.builder
                    .build_conditional_branch(cond_value, then_bb, else_bb)
                    .unwrap();

                self.builder.position_at_end(then_bb);
                self.compile_block(then_block);
                if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
                    self.builder.build_unconditional_branch(merge_bb).unwrap();
                }

                self.builder.position_at_end(else_bb);
                if let Some(else_stmts) = else_block {
                    self.compile_block(else_stmts);
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
                let cond_value = self.compile_expr(cond).into_int_value();
                self.builder
                    .build_conditional_branch(cond_value, body_bb, merge_bb)
                    .unwrap();

                self.builder.position_at_end(body_bb);
                self.compile_block(body);
                if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
                    self.builder.build_unconditional_branch(cond_bb).unwrap();
                }

                self.builder.position_at_end(merge_bb);
            }
        }
    }

    fn compile_expr(&mut self, expr: &Expr) -> BasicValueEnum<'ctx> {
        match expr {
            Expr::Num(n) => self.context.f64_type().const_float(*n).into(),
            Expr::Bool(b) => self.context.bool_type().const_int(*b as u64, false).into(),
            Expr::Str(s) => self.builder.build_global_string_ptr(s, "str").unwrap().as_pointer_value().into(),
            Expr::Var(name) => {
                let (ptr, ty) = self.variables[name];
                self.builder.build_load(ty, ptr, name).unwrap()
            }
            Expr::Unary(op, inner) => {
                let value = self.compile_expr(inner);
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
                let l = self.compile_expr(lhs);
                let r = self.compile_expr(rhs);

                match (l, r) {
                    (BasicValueEnum::FloatValue(lf), BasicValueEnum::FloatValue(rf)) => match op {
                        BinOp::Add => self.builder.build_float_add(lf, rf, "add").unwrap().into(),
                        BinOp::Sub => self.builder.build_float_sub(lf, rf, "sub").unwrap().into(),
                        BinOp::Mul => self.builder.build_float_mul(lf, rf, "mul").unwrap().into(),
                        BinOp::Div => self.builder.build_float_div(lf, rf, "div").unwrap().into(),
                        BinOp::Pow => self
                            .builder
                            .build_call(self.pow_fn, &[lf.into(), rf.into()], "pow")
                            .unwrap()
                            .try_as_basic_value()
                            .basic()
                            .unwrap(),
                        BinOp::Tetration => self.compile_tetration(lf, rf).into(),
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
                    (l, r) => panic!("binary {op:?} used with mismatched operand types {l:?} / {r:?}"),
                }
            }
            Expr::Call(name, args) => {
                let function = self.functions[name];
                let arg_values: Vec<BasicMetadataValueEnum> =
                    args.iter().map(|a| self.compile_expr(a).into()).collect();
                let call = self.builder.build_call(function, &arg_values, "call").unwrap();
                call.try_as_basic_value()
                    .basic()
                    .expect("function used in expression position must return a value")
            }
        }
    }

    /// The printf-style format fragment (no surrounding text) and matching
    /// call argument for a compiled value. Used to build print's combined
    /// format string across all of its segments.
    fn value_fmt(&self, value: BasicValueEnum<'ctx>) -> (&'static str, BasicMetadataValueEnum<'ctx>) {
        match value {
            BasicValueEnum::FloatValue(f) => ("%g", f.into()),
            BasicValueEnum::PointerValue(p) => ("%s", p.into()),
            BasicValueEnum::IntValue(i) => {
                // Only bools (i1) reach here; widen to i64 for the C varargs ABI.
                let widened = self
                    .builder
                    .build_int_z_extend(i, self.context.i64_type(), "fmt_ext")
                    .unwrap();
                ("%lld", widened.into())
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
