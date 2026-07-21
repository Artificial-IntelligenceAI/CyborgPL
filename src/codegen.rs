use std::collections::HashMap;
use std::path::Path;

use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::targets::{CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine};
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue, PointerValue};
use inkwell::{AddressSpace, FloatPredicate, IntPredicate, OptimizationLevel};

use crate::ast::*;

pub struct Codegen<'ctx> {
    context: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    functions: HashMap<String, FunctionValue<'ctx>>,
    variables: HashMap<String, (PointerValue<'ctx>, BasicTypeEnum<'ctx>)>,
    printf_fn: FunctionValue<'ctx>,
}

impl<'ctx> Codegen<'ctx> {
    pub fn new(context: &'ctx Context, module_name: &str) -> Self {
        let module = context.create_module(module_name);
        let builder = context.create_builder();

        // `print` is implemented on top of libc's printf, which we'll link in via `cc`.
        let i8_ptr = context.ptr_type(AddressSpace::default());
        let printf_type = context.i32_type().fn_type(&[i8_ptr.into()], true);
        let printf_fn = module.add_function("printf", printf_type, Some(Linkage::External));

        Codegen {
            context,
            module,
            builder,
            functions: HashMap::new(),
            variables: HashMap::new(),
            printf_fn,
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
    }

    fn declare_function(&mut self, f: &Function) {
        let param_types: Vec<BasicMetadataTypeEnum> =
            f.params.iter().map(|p| self.basic_type(p.ty).into()).collect();

        // The linked C runtime expects `int main(void)`, regardless of what
        // return type our own language's grammar allows on `main`.
        let fn_type = if f.name == "main" {
            self.context.i32_type().fn_type(&param_types, false)
        } else {
            match f.return_type {
                Type::Num => self.context.f64_type().fn_type(&param_types, false),
                Type::Bool => self.context.bool_type().fn_type(&param_types, false),
                Type::Str => self.context.ptr_type(AddressSpace::default()).fn_type(&param_types, false),
                Type::Void => self.context.void_type().fn_type(&param_types, false),
            }
        };

        let function = self.module.add_function(&f.name, fn_type, None);
        self.functions.insert(f.name.clone(), function);
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
            if f.name == "main" {
                let zero = self.context.i32_type().const_int(0, false);
                self.builder.build_return(Some(&zero)).unwrap();
            } else {
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
            Stmt::Print(expr) => {
                let value = self.compile_expr(expr);
                let (fmt_str, arg): (&str, BasicMetadataValueEnum) = match value {
                    BasicValueEnum::FloatValue(f) => ("%g\n", f.into()),
                    BasicValueEnum::PointerValue(p) => ("%s\n", p.into()),
                    BasicValueEnum::IntValue(i) => {
                        // Only bools (i1) reach here; widen to i64 for printf's varargs.
                        let widened = self
                            .builder
                            .build_int_z_extend(i, self.context.i64_type(), "print_ext")
                            .unwrap();
                        ("%lld\n", widened.into())
                    }
                    other => panic!("print: unsupported value {other:?}"),
                };
                let fmt = self.builder.build_global_string_ptr(fmt_str, "fmt").unwrap();
                self.builder
                    .build_call(self.printf_fn, &[fmt.as_pointer_value().into(), arg], "printf_call")
                    .unwrap();
            }
            Stmt::ExprStmt(expr) => {
                self.compile_expr(expr);
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

    fn current_function(&self) -> FunctionValue<'ctx> {
        self.builder
            .get_insert_block()
            .unwrap()
            .get_parent()
            .unwrap()
    }
}
