use std::collections::HashMap;
use std::path::Path;

use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FloatType, IntType, StructType};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FloatValue, FunctionValue, IntValue, PointerValue};
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
    pow: FunctionValue<'ctx>,
    /// Truncates a bignum to a native i64 -- used to turn a tetration
    /// height into a loop trip count.
    get_i64: FunctionValue<'ctx>,
    neg: FunctionValue<'ctx>,
    /// mpf_cmp's own convention (negative/zero/positive) -- codegen just
    /// compares this against 0 with whichever predicate the source asked for.
    cmp: FunctionValue<'ctx>,
}

/// The GMP shim functions (runtime/gmp/bigint_shim.c) backing `bigint`.
/// No `precision`/width parameter anywhere here -- unlike `BignumFns`,
/// which needs one for `new`, `bigint` is unbounded.
struct BigIntFns<'ctx> {
    new: FunctionValue<'ctx>,
    set_str: FunctionValue<'ctx>,
    copy: FunctionValue<'ctx>,
    add: FunctionValue<'ctx>,
    sub: FunctionValue<'ctx>,
    mul: FunctionValue<'ctx>,
    div: FunctionValue<'ctx>,
    to_string: FunctionValue<'ctx>,
    free: FunctionValue<'ctx>,
    pow: FunctionValue<'ctx>,
    /// `a xxx b`, computed entirely inside the shim (unlike bignum's own
    /// tetration, which builds the loop as LLVM IR) -- see
    /// runtime/gmp/bigint_shim.c for why. Takes the height directly as a
    /// native `i64`, not another bigint handle.
    tetration: FunctionValue<'ctx>,
    /// Postfix `!`, likewise computed entirely inside the shim via GMP's
    /// own `mpz_fac_ui` rather than a hand-rolled loop.
    factorial: FunctionValue<'ctx>,
    neg: FunctionValue<'ctx>,
    /// mpz_cmp's own convention (negative/zero/positive) -- codegen just
    /// compares this against 0 with whichever predicate the source asked for.
    cmp: FunctionValue<'ctx>,
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
    /// Each function's declared parameter types (in order) and return type
    /// -- used at every call site to coerce arguments to what the callee
    /// actually expects, and to know whether the result needs bignum
    /// lifetime tracking. Populated in `declare_function`, alongside
    /// `functions`, before any function body is compiled.
    function_sigs: HashMap<String, (Vec<Type>, Type)>,
    /// The return type of whichever function is currently being compiled
    /// (`Type::Void` for the entry block, which has no return type of its
    /// own). `Stmt::Return` coerces its value against this before handing
    /// it back, the same way a variable's declared type coerces whatever
    /// is stored into it.
    current_return_type: Type,
    /// Whether `Stmt::Return` is currently being compiled inside
    /// `START...END` itself, rather than a real user function. `main`'s
    /// actual LLVM return type is always `i32` (the C runtime's own
    /// calling convention), regardless of `current_return_type` being
    /// `Type::Void` here (there's no source-level return type for the
    /// entry point to track) -- so a bare `return;` written inside
    /// `START...END` needs to build `ret i32 0`, matching the
    /// fall-off-the-end case `compile_entry` already handles, not a real
    /// `ret void` (which would leave `main`'s return-value register
    /// unset -- an undefined, garbage process exit code, not the
    /// process crashing or misbehaving otherwise).
    in_entry: bool,
    /// Keyed by (name, type) rather than just name, so a name can be shared
    /// by variables of different types -- ref:var:TYPE 'name' picks between
    /// them by type at each reference site.
    variables: HashMap<(String, Type), (PointerValue<'ctx>, BasicTypeEnum<'ctx>)>,
    /// Stack of block scopes, innermost last. Each `compile_block` call
    /// pushes one frame and pops it when the block ends, restoring
    /// whatever `variables` looked like before that block ran.
    scopes: Vec<Vec<ScopeEntry<'ctx>>>,
    /// Bignum handles allocated as *intermediate* expression results
    /// (binary/unary ops, a bignum-returning call) during the statement
    /// currently being compiled -- never a named variable's own handle.
    /// Drained and freed once that statement finishes, unless
    /// `compile_and_coerce` adopts one directly instead (see its own
    /// docs) -- every other consumer (coerce_to_bignum's copy path,
    /// bignum_to_string, an enclosing binary op) only reads/copies,
    /// never adopts the pointer. Each entry keeps the raw handle (to
    /// free), the exact wrapped value produced for it (`build_extract_value`
    /// makes a *new* instruction every time it's called, even reading the
    /// same field back out of the same struct -- so identifying "is this
    /// literally the value this statement is returning" has to compare
    /// against that original produced value, not a freshly re-extracted
    /// pointer, or the comparison never matches), and the precision the
    /// handle actually holds (almost always `DEFAULT_BIGNUM_PRECISION`,
    /// except a call to a function whose own declared return precision
    /// differs) -- needed so adopting a temp directly can confirm its
    /// precision genuinely matches the target before skipping the copy.
    bignum_temps: Vec<(PointerValue<'ctx>, BasicValueEnum<'ctx>, u32)>,
    /// `str` values produced by `stch` or a str-returning call, not yet
    /// adopted by a variable/return -- simpler than `bignum_temps` since
    /// `str` is a bare pointer already (no struct-wrapping identity concern).
    /// Drained and freed the same way, at the end of the statement that
    /// produced them (or earlier, by `Stmt::Return`, once superseded by its
    /// own always-fresh copy).
    str_temps: Vec<PointerValue<'ctx>>,
    /// One frame per `while` loop currently being compiled (innermost
    /// last), each mapping a bare literal's exact value (`f64::to_bits`,
    /// since `f64` isn't `Hash`/`Eq`) and the bignum precision it's paired
    /// with to the synthetic scoped variable holding its *already
    /// constructed* handle. Populated by `Stmt::While`'s own codegen
    /// (`find_hoistable_bignum_literals`) right before the loop, so a
    /// literal combined with a bignum inside the loop body -- e.g.
    /// `acc + (1)` -- doesn't otherwise pay for a fresh `bignum_new` +
    /// `set_d` (a real GMP malloc) on every single iteration: GMP calls
    /// aren't pure, so LLVM's own optimizer never hoists this for us.
    /// Each frame is scoped exactly like a block's own local variables
    /// (pushed/popped alongside a dedicated `scopes` frame), so an early
    /// `return` from inside the loop still frees it correctly via
    /// `Stmt::Return`'s existing "free every open scope" pass.
    hoisted_bignum_literals: Vec<HashMap<(u64, u32), (String, Type)>>,
    /// Source of unique names for hoisted-literal synthetic variables --
    /// never seen or written by any CyborgPL program, just needs to never
    /// collide with another hoist or with itself across loops.
    next_hoisted_lit_id: u32,
    printf_fn: FunctionValue<'ctx>,
    /// libm's `pow`, backing both `xx` (power) and `xxx` (tetration).
    pow_fn: FunctionValue<'ctx>,
    /// libc's `free` -- used for `bignum_to_string`'s returned buffer, and
    /// now any owned `str` buffer (a variable's strdup'd copy, or a `stch`
    /// result) -- never for an actual bignum handle (that's `bignum.free`,
    /// which also runs `mpf_clear` first).
    libc_free: FunctionValue<'ctx>,
    /// libc's `malloc`/`snprintf`, backing `stch`'s two-pass "measure, then
    /// fill" string build. libc's `strdup`, giving every `str` *stored*
    /// somewhere (a variable, a return, a call argument) its own
    /// independent heap copy -- the same "always copy on store" rule
    /// `bignum` already follows -- so a `str` variable's buffer can always
    /// be freed unconditionally at scope exit, with no need to track
    /// whether it started life as a literal or a `stch` result.
    malloc_fn: FunctionValue<'ctx>,
    snprintf_fn: FunctionValue<'ctx>,
    strdup_fn: FunctionValue<'ctx>,
    /// runtime/io/input_shim.c -- `input:str`/`input:num`, reading a line
    /// from stdin. `read_line_fn`'s result is already a fresh malloc'd
    /// buffer (getline's own allocation), adopted directly as a `str`
    /// value with no extra strdup needed.
    read_line_fn: FunctionValue<'ctx>,
    read_num_fn: FunctionValue<'ctx>,
    /// Extracted out of `cyborg_read_num` so `input:num [from*(dest)*];`
    /// (parsing a whole file's content) can reuse the exact same
    /// validation `cyborg_read_num` (stdin) already does.
    parse_num_or_die_fn: FunctionValue<'ctx>,
    /// runtime/clock/clock_shim.c -- `clock:num`, elapsed seconds since
    /// the program started (captured once, before `main`, not from the
    /// first `clock:num` read).
    clock_elapsed_fn: FunctionValue<'ctx>,
    /// runtime/io/file_shim.c -- `cyborg_fopen_or_die` backs `print`'s
    /// (optional) and `overwrite`'s (required) `[to*(dest)*]` clause;
    /// crashes with a clear message rather than codegen having to emit its
    /// own null-check IR. `fprintf_fn`/`fclose_fn` are plain libc.
    fopen_or_die_fn: FunctionValue<'ctx>,
    fprintf_fn: FunctionValue<'ctx>,
    fclose_fn: FunctionValue<'ctx>,
    /// `input:`'s `[from*(dest)*]` clause -- reads dest's whole content
    /// into a fresh owned buffer, adopted directly as `str` (or parsed via
    /// `parse_num_or_die_fn` for `num`), same "crash with a clear message"
    /// failure mode as opening a file for writing already has.
    read_file_fn: FunctionValue<'ctx>,
    /// runtime/array/array_shim.c -- `var:array:TYPE`'s growable, type-erased
    /// (element size in bytes only) backing buffer.
    array_new_fn: FunctionValue<'ctx>,
    array_free_fn: FunctionValue<'ctx>,
    array_append_fn: FunctionValue<'ctx>,
    array_get_ptr_fn: FunctionValue<'ctx>,
    array_length_fn: FunctionValue<'ctx>,
    /// runtime/int/int_shim.c -- the single shared crash-with-message
    /// path backing every "int can't represent this result" case
    /// (overflowing +/-/x/xx/xxx/!, division by zero, negating i64::MIN).
    int_die_fn: FunctionValue<'ctx>,
    /// `llvm.s{add,sub,mul}.with.overflow.i64` -- each returns `{i64,
    /// i1}` (result, did-it-overflow), letting int's arithmetic detect
    /// overflow directly instead of silently wrapping (two's-complement).
    sadd_overflow_fn: FunctionValue<'ctx>,
    ssub_overflow_fn: FunctionValue<'ctx>,
    smul_overflow_fn: FunctionValue<'ctx>,
    bignum: BignumFns<'ctx>,
    bigint: BigIntFns<'ctx>,
    /// The distinct named `{ptr}` struct type every `bigint` value is
    /// wrapped in -- see its own construction site in `new` for why this
    /// has to be a separate, named type rather than reusing
    /// `bignum_struct_type()`'s anonymous one.
    bigint_struct_ty: StructType<'ctx>,
    /// Same role as `bignum_temps`, for `bigint` -- intermediate handles
    /// (binary/unary op results, a bigint-returning call) not yet
    /// adopted by a variable/return, freed once the statement that
    /// produced them is done with them. Simpler than `bignum_temps`:
    /// no precision to track alongside each entry.
    bigint_temps: Vec<(PointerValue<'ctx>, BasicValueEnum<'ctx>)>,
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

        let free_type = context.void_type().fn_type(&[i8_ptr.into()], false);
        let libc_free = module.add_function("free", free_type, Some(Linkage::External));

        let i64_type = context.i64_type();
        let malloc_type = i8_ptr.fn_type(&[i64_type.into()], false);
        let malloc_fn = module.add_function("malloc", malloc_type, Some(Linkage::External));

        let snprintf_type = context.i32_type().fn_type(&[i8_ptr.into(), i64_type.into(), i8_ptr.into()], true);
        let snprintf_fn = module.add_function("snprintf", snprintf_type, Some(Linkage::External));

        let strdup_type = i8_ptr.fn_type(&[i8_ptr.into()], false);
        let strdup_fn = module.add_function("strdup", strdup_type, Some(Linkage::External));

        let read_line_type = i8_ptr.fn_type(&[], false);
        let read_line_fn = module.add_function("cyborg_read_line", read_line_type, Some(Linkage::External));

        let read_num_type = f64_type.fn_type(&[], false);
        let read_num_fn = module.add_function("cyborg_read_num", read_num_type, Some(Linkage::External));

        let parse_num_or_die_type = f64_type.fn_type(&[i8_ptr.into()], false);
        let parse_num_or_die_fn =
            module.add_function("cyborg_parse_num_or_die", parse_num_or_die_type, Some(Linkage::External));

        let clock_elapsed_type = f64_type.fn_type(&[], false);
        let clock_elapsed_fn = module.add_function("cyborg_clock_elapsed", clock_elapsed_type, Some(Linkage::External));

        let fopen_or_die_type = i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into()], false);
        let fopen_or_die_fn = module.add_function("cyborg_fopen_or_die", fopen_or_die_type, Some(Linkage::External));

        let fprintf_type = context.i32_type().fn_type(&[i8_ptr.into(), i8_ptr.into()], true);
        let fprintf_fn = module.add_function("fprintf", fprintf_type, Some(Linkage::External));

        let fclose_type = context.i32_type().fn_type(&[i8_ptr.into()], false);
        let fclose_fn = module.add_function("fclose", fclose_type, Some(Linkage::External));

        let read_file_type = i8_ptr.fn_type(&[i8_ptr.into()], false);
        let read_file_fn = module.add_function("cyborg_read_file_or_die", read_file_type, Some(Linkage::External));

        // runtime/array/array_shim.c -- type-erased (element size in bytes
        // only); codegen does its own load/store through the raw slot
        // pointer cyborg_array_get_ptr returns, since opaque pointers mean
        // no cast is needed regardless of the element's actual type.
        let array_i64_ty = context.i64_type();
        let array_new_type = i8_ptr.fn_type(&[array_i64_ty.into()], false);
        let array_new_fn = module.add_function("cyborg_array_new", array_new_type, Some(Linkage::External));

        let array_free_type = context.void_type().fn_type(&[i8_ptr.into()], false);
        let array_free_fn = module.add_function("cyborg_array_free", array_free_type, Some(Linkage::External));

        let array_append_type = context.void_type().fn_type(&[i8_ptr.into(), i8_ptr.into()], false);
        let array_append_fn = module.add_function("cyborg_array_append", array_append_type, Some(Linkage::External));

        let array_get_ptr_type = i8_ptr.fn_type(&[i8_ptr.into(), array_i64_ty.into()], false);
        let array_get_ptr_fn = module.add_function("cyborg_array_get_ptr", array_get_ptr_type, Some(Linkage::External));

        let array_length_type = array_i64_ty.fn_type(&[i8_ptr.into()], false);
        let array_length_fn = module.add_function("cyborg_array_length", array_length_type, Some(Linkage::External));

        // runtime/int/int_shim.c
        let int_die_type = context.void_type().fn_type(&[i8_ptr.into()], false);
        let int_die_fn = module.add_function("cyborg_int_die", int_die_type, Some(Linkage::External));

        // LLVM's overflow-checked arithmetic intrinsics -- declared with
        // their exact mangled names and a `{i64, i1}` (result, overflowed)
        // return type, the same way a hand-written .ll file would; no
        // special "intrinsic" API needed beyond declaring the right
        // name/signature and calling it like any other function.
        let overflow_result_ty = context.struct_type(&[i64_type.into(), context.bool_type().into()], false);
        let overflow_fn_type = overflow_result_ty.fn_type(&[i64_type.into(), i64_type.into()], false);
        let sadd_overflow_fn =
            module.add_function("llvm.sadd.with.overflow.i64", overflow_fn_type, Some(Linkage::External));
        let ssub_overflow_fn =
            module.add_function("llvm.ssub.with.overflow.i64", overflow_fn_type, Some(Linkage::External));
        let smul_overflow_fn =
            module.add_function("llvm.smul.with.overflow.i64", overflow_fn_type, Some(Linkage::External));

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
            pow: module.add_function(
                "bignum_pow",
                void_ty.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
                Some(Linkage::External),
            ),
            get_i64: module.add_function(
                "bignum_get_i64",
                i64_ty.fn_type(&[i8_ptr.into()], false),
                Some(Linkage::External),
            ),
            neg: module.add_function(
                "bignum_neg",
                void_ty.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
                Some(Linkage::External),
            ),
            cmp: module.add_function(
                "bignum_cmp",
                context.i32_type().fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
                Some(Linkage::External),
            ),
        };

        // runtime/gmp/bigint_shim.c -- same opaque-i8_ptr-handle
        // convention as bignum above, but `new` takes no precision
        // argument at all (bigint is unbounded).
        let bigint = BigIntFns {
            new: module.add_function("bigint_new", i8_ptr.fn_type(&[], false), Some(Linkage::External)),
            set_str: module.add_function(
                "bigint_set_str",
                void_ty.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
                Some(Linkage::External),
            ),
            copy: module.add_function(
                "bigint_copy",
                void_ty.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
                Some(Linkage::External),
            ),
            add: module.add_function(
                "bigint_add",
                void_ty.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
                Some(Linkage::External),
            ),
            sub: module.add_function(
                "bigint_sub",
                void_ty.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
                Some(Linkage::External),
            ),
            mul: module.add_function(
                "bigint_mul",
                void_ty.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
                Some(Linkage::External),
            ),
            div: module.add_function(
                "bigint_div",
                void_ty.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
                Some(Linkage::External),
            ),
            to_string: module.add_function(
                "bigint_to_string",
                i8_ptr.fn_type(&[i8_ptr.into()], false),
                Some(Linkage::External),
            ),
            free: module.add_function(
                "bigint_free",
                void_ty.fn_type(&[i8_ptr.into()], false),
                Some(Linkage::External),
            ),
            pow: module.add_function(
                "bigint_pow",
                void_ty.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
                Some(Linkage::External),
            ),
            tetration: module.add_function(
                "bigint_tetration",
                void_ty.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
                Some(Linkage::External),
            ),
            factorial: module.add_function(
                "bigint_factorial",
                void_ty.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
                Some(Linkage::External),
            ),
            neg: module.add_function(
                "bigint_neg",
                void_ty.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
                Some(Linkage::External),
            ),
            cmp: module.add_function(
                "bigint_cmp",
                context.i32_type().fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
                Some(Linkage::External),
            ),
        };

        // A distinct (named, not anonymous) single-field `{ptr}` struct
        // type for `bigint` -- structurally identical to bignum/array's
        // shared anonymous `{ptr}` wrapper, but a genuinely *different*
        // LLVM type. Needed because LLVM structurally unifies anonymous
        // struct types (two separately-built anonymous `{ptr}` structs
        // are literally the same type), so a second anonymous wrapper
        // could never be told apart from bignum's own at a site that
        // dispatches on value *shape* alone (`value_fmt`, in particular)
        // with no accompanying static type available. Built once, here,
        // and reused (never reconstructed) so every value built/compared
        // anywhere in codegen actually shares the same type.
        let bigint_struct_ty = context.opaque_struct_type("bigint_handle");
        bigint_struct_ty.set_body(&[i8_ptr.into()], false);

        Codegen {
            context,
            module,
            builder,
            functions: HashMap::new(),
            function_sigs: HashMap::new(),
            current_return_type: Type::Void,
            in_entry: false,
            variables: HashMap::new(),
            scopes: Vec::new(),
            bignum_temps: Vec::new(),
            str_temps: Vec::new(),
            hoisted_bignum_literals: Vec::new(),
            next_hoisted_lit_id: 0,
            printf_fn,
            pow_fn,
            libc_free,
            malloc_fn,
            snprintf_fn,
            strdup_fn,
            read_line_fn,
            read_num_fn,
            parse_num_or_die_fn,
            clock_elapsed_fn,
            fopen_or_die_fn,
            fprintf_fn,
            fclose_fn,
            read_file_fn,
            array_new_fn,
            array_free_fn,
            array_append_fn,
            array_get_ptr_fn,
            array_length_fn,
            int_die_fn,
            sadd_overflow_fn,
            ssub_overflow_fn,
            smul_overflow_fn,
            bignum,
            bigint,
            bigint_struct_ty,
            bigint_temps: Vec::new(),
        }
    }

    pub fn module(&self) -> &Module<'ctx> {
        &self.module
    }

    /// Lower the LLVM IR built up so far into a native `.o` object file,
    /// targeting whatever machine this compiler itself is running on.
    /// `optimize: false` (the CLI's `-O0`) skips LLVM's optimizer
    /// entirely -- not just "beyond mem2reg", genuinely nothing at all,
    /// the same real `-O0` meaning every other compiler (clang, rustc,
    /// gcc) already uses: raw alloca/store/load exactly as codegen
    /// emitted it, and every loop actually runs every iteration rather
    /// than the optimizer proving some of them away. Useful for an
    /// honest look at what a program's code/timing genuinely is, without
    /// LLVM quietly doing (or undoing) work on your behalf.
    pub fn write_object_file(&self, path: &Path, optimize: bool) -> Result<(), String> {
        Target::initialize_native(&InitializationConfig::default())?;

        let triple = TargetMachine::get_default_triple();
        let target = Target::from_triple(&triple).map_err(|e| e.to_string())?;
        let cpu = TargetMachine::get_host_cpu_name().to_string();
        let features = TargetMachine::get_host_cpu_features().to_string();

        let opt_level = if optimize { OptimizationLevel::Default } else { OptimizationLevel::None };
        let target_machine = target
            .create_target_machine(&triple, &cpu, &features, opt_level, RelocMode::PIC, CodeModel::Default)
            .ok_or("failed to create target machine for this host")?;

        // The full standard -O2 pipeline (mem2reg/SROA, inlining, GVN,
        // dead-code elimination, instcombine, loop optimizations, etc.) --
        // LLVM's own well-tested optimizer, not anything hand-picked or
        // CyborgPL-specific. Free in the sense that it costs nothing to
        // write (one pipeline name, not a list of passes to choose and
        // maintain) and applies uniformly to every compiled program,
        // unlike this project's own targeted optimizations (bignum chain
        // fusion, literal hoisting, etc.), which each only help the
        // specific pattern they were built for.
        if optimize {
            self.module
                .run_passes("default<O2>", &target_machine, PassBuilderOptions::create())
                .map_err(|e| e.to_string())?;
        }

        target_machine
            .write_to_file(&self.module, FileType::Object, path)
            .map_err(|e| e.to_string())
    }

    fn basic_type(&self, ty: Type) -> BasicTypeEnum<'ctx> {
        match ty {
            Type::Num(width) => self.float_type_for(width).into(),
            Type::NumW(width) => self.float_type_for(width).into(),
            Type::Bool => self.context.bool_type().into(),
            Type::Int(width) => self.int_type_for(width).into(),
            Type::Str | Type::File => self.context.ptr_type(AddressSpace::default()).into(),
            // Same wrapped-pointer shape as `BigNum` -- reused directly,
            // not a second copy of the same trick. Structurally identical
            // to bignum at the LLVM level (an unnamed `{ptr}` struct type
            // is the same type either way); the type checker is the only
            // thing keeping the two from ever being confused, same as it
            // already keeps `bignum` and `str` (a bare pointer) apart.
            Type::BigNum(_) | Type::Array(_) => self.bignum_struct_type().into(),
            // A genuinely different wrapped-pointer struct type than
            // bignum/array's -- see bigint_struct_ty's own construction
            // site (in `new`) for why it can't just reuse theirs.
            Type::BigInt => self.bigint_struct_ty.into(),
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

    /// Same wrapping trick as `wrap_bignum_ptr`, but using `bigint`'s own
    /// distinct struct type -- see `bigint_struct_ty`'s construction site
    /// for why the two can't share one.
    fn wrap_bigint_ptr(&self, ptr: PointerValue<'ctx>) -> BasicValueEnum<'ctx> {
        let undef = self.bigint_struct_ty.get_undef();
        self.builder.build_insert_value(undef, ptr, 0, "bigint_wrap").unwrap().into_struct_value().into()
    }

    fn unwrap_bigint_ptr(&self, value: BasicValueEnum<'ctx>) -> PointerValue<'ctx> {
        self.builder
            .build_extract_value(value.into_struct_value(), 0, "bigint_ptr")
            .unwrap()
            .into_pointer_value()
    }

    /// Calls bigint_new() and returns the resulting handle pointer (not
    /// yet wrapped) -- no precision argument, unlike `bignum_new`.
    fn bigint_new(&self) -> PointerValue<'ctx> {
        self.builder
            .build_call(self.bigint.new, &[], "bigint_new_call")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value()
    }

    /// Converts an already-compiled value into a *freshly allocated*
    /// bigint -- always a fresh handle and a copy, mirroring
    /// `coerce_to_bignum` (bigint is heap-backed but has to behave *by
    /// value* at the language level, same as every other type here).
    /// Only ever reached with a `PointerValue` (a literal's raw text) or
    /// a `StructValue` (another bigint) -- `bigint` is isolated, so
    /// nothing else can reach this.
    fn coerce_to_bigint(&self, value: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        let handle = self.bigint_new();
        match value {
            BasicValueEnum::PointerValue(p) => {
                self.builder.build_call(self.bigint.set_str, &[handle.into(), p.into()], "bigint_set_str_call").unwrap();
            }
            BasicValueEnum::StructValue(_) => {
                let src = self.unwrap_bigint_ptr(value);
                self.builder.build_call(self.bigint.copy, &[handle.into(), src.into()], "bigint_copy_call").unwrap();
            }
            other => panic!("cannot use {other:?} as a bigint value"),
        }
        self.wrap_bigint_ptr(handle)
    }

    /// Whether `value` is specifically a `bigint` (as opposed to a
    /// `bignum`/`array`, which share the *shape* `StructValue` but not
    /// the exact LLVM type) -- the one place this distinction has to be
    /// made from a bare runtime value alone, with no accompanying static
    /// `Expr`/`Type` to consult (`value_fmt`, called deep inside
    /// print/`stch` compiling).
    fn is_bigint_value(&self, value: BasicValueEnum<'ctx>) -> bool {
        matches!(value, BasicValueEnum::StructValue(sv) if sv.get_type() == self.bigint_struct_ty)
    }

    /// Determines what precision an expression, already known (or about
    /// to be used) as a `bignum`, actually gets constructed at -- purely
    /// via static AST inspection, mirroring typecheck.rs's own precision
    /// computation exactly (the same "widen to the larger operand" rule
    /// `check_binary` now applies, the same "a promoted float takes on
    /// the bignum side's own precision" rule `coerce_to_bignum` already
    /// follows, and the same "Neg preserves, Factorial always defaults"
    /// split `Expr::Unary` already has). No runtime GMP query is needed
    /// for this, unlike `int`'s overflow checking: a bignum's precision
    /// is fixed once at construction (`bignum_new`) and never changes
    /// afterward, so it's a purely static, type-level property -- as
    /// long as this stays a faithful mirror of typecheck's computation,
    /// codegen's constructed precision and the type checker's reported
    /// one can never drift apart. `None` means `expr` isn't bignum-shaped
    /// at all (e.g. a plain `num` -- relevant only while recursing into a
    /// `Binary` node that might mix the two).
    fn bignum_precision_of_expr(&self, expr: &Expr) -> Option<u32> {
        match expr {
            Expr::Var(_, Type::BigNum(p)) => Some(*p),
            Expr::ArrayIndex(_, Type::Array(ElementType::BigNum(p)), _) => Some(*p),
            Expr::Call(name, _) => match self.function_sigs.get(name) {
                Some((_, Type::BigNum(p))) => Some(*p),
                _ => None,
            },
            Expr::Unary(UnOp::Neg, inner) => self.bignum_precision_of_expr(inner),
            Expr::Unary(UnOp::Factorial, inner) => {
                self.bignum_precision_of_expr(inner).map(|_| DEFAULT_BIGNUM_PRECISION)
            }
            Expr::Binary(lhs, op, rhs) if *op != BinOp::Concat => {
                match (self.bignum_precision_of_expr(lhs), self.bignum_precision_of_expr(rhs)) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (Some(a), None) | (None, Some(a)) => Some(a),
                    (None, None) => None,
                }
            }
            _ => None,
        }
    }

    /// Whether `expr` statically resolves to `bigint` -- mirrors
    /// `bignum_precision_of_expr`'s structure exactly, just a bool
    /// instead of a precision (there's nothing to widen to). Needed
    /// because `bigint` reuses the *shape* `StructValue` that
    /// `bignum`/`array` already use at the LLVM level (see
    /// `bigint_struct_ty`), so `compile_expr`'s `Expr::Binary`/`Unary`
    /// arms can't tell a `bigint` operand apart from a `bignum` one by
    /// runtime shape alone -- this has to be decided from the AST,
    /// before either side is even compiled, the same way `int`'s
    /// literal-pairing already has to be resolved structurally rather
    /// than from a compiled value's shape (which would just be `f64`
    /// either way).
    fn expr_is_bigint(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Var(_, Type::BigInt) => true,
            Expr::ArrayIndex(_, Type::Array(ElementType::BigInt), _) => true,
            Expr::Call(name, _) => matches!(self.function_sigs.get(name), Some((_, Type::BigInt))),
            Expr::Unary(UnOp::Neg | UnOp::Factorial, inner) => self.expr_is_bigint(inner),
            Expr::Binary(lhs, op, rhs) if *op != BinOp::Concat => {
                self.expr_is_bigint(lhs) || self.expr_is_bigint(rhs)
            }
            _ => false,
        }
    }

    /// Scans every statement `block` directly contains -- recursing into
    /// `If`'s own branches, but never into a nested `While`'s condition or
    /// body -- for every point where `compile_expr`'s `Expr::Binary`
    /// literal-pairing dispatch would otherwise construct a fresh bignum
    /// handle (a real GMP malloc) for a bare literal on every single
    /// evaluation. A nested loop hoists independently, scoped to its own
    /// preheader, once `compile_stmt` actually reaches it -- recursing
    /// into it here would hoist its literals to the *wrong* (outer)
    /// scope, freed too late relative to the inner loop that actually
    /// uses them. Returns each literal's raw value (as `f64::to_bits`,
    /// since `f64` isn't `Hash`/`Eq`) paired with the bignum precision it
    /// would be built at.
    fn find_hoistable_bignum_literals(&self, block: &Block, out: &mut Vec<(u64, u32)>) {
        for stmt in block {
            match stmt {
                Stmt::VarDecl(_, _, e) | Stmt::Assign(_, _, e) | Stmt::ExprStmt(e) => {
                    self.scan_expr_for_bignum_literals(e, out);
                }
                Stmt::ArrayIndexAssign(_, _, idx, val) => {
                    self.scan_expr_for_bignum_literals(idx, out);
                    self.scan_expr_for_bignum_literals(val, out);
                }
                Stmt::Append(arr, val) => {
                    self.scan_expr_for_bignum_literals(arr, out);
                    self.scan_expr_for_bignum_literals(val, out);
                }
                Stmt::Return(Some(e)) => self.scan_expr_for_bignum_literals(e, out),
                // `Read`'s source is always str/file, never bignum-shaped,
                // so there's nothing here worth scanning for.
                Stmt::Return(None) | Stmt::Input(..) | Stmt::Clock(..) | Stmt::While(..) | Stmt::Read(_) => {}
                Stmt::Print(segments, dest) => {
                    for seg in segments {
                        if let PrintSegment::Expr(e) = seg {
                            self.scan_expr_for_bignum_literals(e, out);
                        }
                    }
                    if let Some(d) = dest {
                        self.scan_expr_for_bignum_literals(d, out);
                    }
                }
                Stmt::Overwrite(segments, dest) => {
                    for seg in segments {
                        if let PrintSegment::Expr(e) = seg {
                            self.scan_expr_for_bignum_literals(e, out);
                        }
                    }
                    self.scan_expr_for_bignum_literals(dest, out);
                }
                Stmt::If(cond, then_b, else_b) => {
                    self.scan_expr_for_bignum_literals(cond, out);
                    self.find_hoistable_bignum_literals(then_b, out);
                    if let Some(eb) = else_b {
                        self.find_hoistable_bignum_literals(eb, out);
                    }
                }
            }
        }
    }

    /// Same literal-pairing shape `compile_expr`'s `Expr::Binary` arm
    /// matches (mirrored exactly, including the "not also a literal on
    /// the other side" guard) -- recurses through every other `Expr`
    /// position so a literal nested arbitrarily deep still gets found.
    fn scan_expr_for_bignum_literals(&self, expr: &Expr, out: &mut Vec<(u64, u32)>) {
        match expr {
            Expr::Binary(lhs, op, rhs) if *op != BinOp::Concat => match (lhs.as_ref(), rhs.as_ref()) {
                (Expr::Num(n, _), other) if !matches!(other, Expr::Num(_, _)) => {
                    if let Some(p) = self.bignum_precision_of_expr(other) {
                        out.push((n.to_bits(), p));
                    }
                    self.scan_expr_for_bignum_literals(other, out);
                }
                (other, Expr::Num(n, _)) if !matches!(other, Expr::Num(_, _)) => {
                    if let Some(p) = self.bignum_precision_of_expr(other) {
                        out.push((n.to_bits(), p));
                    }
                    self.scan_expr_for_bignum_literals(other, out);
                }
                _ => {
                    self.scan_expr_for_bignum_literals(lhs, out);
                    self.scan_expr_for_bignum_literals(rhs, out);
                }
            },
            Expr::Binary(lhs, _, rhs) => {
                self.scan_expr_for_bignum_literals(lhs, out);
                self.scan_expr_for_bignum_literals(rhs, out);
            }
            Expr::Unary(_, inner) => self.scan_expr_for_bignum_literals(inner, out),
            Expr::Call(_, args) => {
                for a in args {
                    self.scan_expr_for_bignum_literals(a, out);
                }
            }
            Expr::ArrayLiteral(elems) => {
                for e in elems {
                    self.scan_expr_for_bignum_literals(e, out);
                }
            }
            Expr::ArrayIndex(_, _, idx) => self.scan_expr_for_bignum_literals(idx, out),
            Expr::Length(e) => self.scan_expr_for_bignum_literals(e, out),
            Expr::Num(_, _) | Expr::Bool(_) | Expr::Str(_) | Expr::Var(_, _) => {}
        }
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

    fn int_type_for(&self, width: u32) -> IntType<'ctx> {
        match width {
            8 => self.context.i8_type(),
            16 => self.context.i16_type(),
            32 => self.context.i32_type(),
            64 => self.context.i64_type(),
            other => panic!("unsupported int precision: {other} (the parser should have rejected this)"),
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
    /// precision, converts it. No-op for bool. A `str` reaching *this*
    /// function always gets its own fresh `strdup`'d copy -- same "always
    /// an independent copy on store" rule `bignum` already follows -- so a
    /// `str` variable's buffer can always be freed unconditionally at
    /// scope exit without having to track whether it started out as a
    /// literal (never to be freed) or a `stch` result (already
    /// heap-owned). `compile_and_coerce` -- the only caller -- skips this
    /// entirely and adopts the handle directly whenever it's already a
    /// not-yet-consumed `str_temps` entry, the same "nothing else
    /// references it" shortcut `bignum` already gets; a plain literal or
    /// variable read never is one, so this copy path is exactly the
    /// remaining case that genuinely still needs it.
    fn coerce_to_type(&self, value: BasicValueEnum<'ctx>, ty: Type) -> BasicValueEnum<'ctx> {
        match (value, ty) {
            (BasicValueEnum::FloatValue(f), Type::Num(width)) => self.coerce_float(f, width).into(),
            (BasicValueEnum::FloatValue(f), Type::NumW(width)) => self.coerce_float(f, width).into(),
            (BasicValueEnum::IntValue(iv), Type::Int(width)) => self.coerce_int_width(iv, width).into(),
            (_, Type::BigNum(precision)) => self.coerce_to_bignum(value, precision),
            (_, Type::BigInt) => self.coerce_to_bigint(value),
            (BasicValueEnum::PointerValue(p), Type::Str | Type::File) => self
                .builder
                .build_call(self.strdup_fn, &[p.into()], "str_own_call")
                .unwrap()
                .try_as_basic_value()
                .basic()
                .unwrap(),
            (_, Type::Array(elem)) => self.coerce_to_array(value, elem),
            _ => value,
        }
    }

    /// Converts an `int` value to a *different* declared width -- a
    /// no-op if it's already that width. Widening (moving to a *wider*
    /// width) is always exact, a plain sign-extend. Narrowing is
    /// overflow-checked: truncate, then sign-extend the truncated value
    /// back up to the original width and compare against the original --
    /// if they differ, the value didn't actually fit in the narrower
    /// width, and it crashes with a clear message rather than silently
    /// wrapping (two's-complement). This is the one place `int`'s
    /// per-width safety is enforced; arithmetic itself always happens at
    /// a full i64 internally (see `match_int_widths`/`compile_int_expr`),
    /// so this is where that gets reconciled with whatever width the
    /// value is actually being stored as.
    fn coerce_int_width(&self, value: IntValue<'ctx>, target_width: u32) -> IntValue<'ctx> {
        let current_width = value.get_type().get_bit_width();
        if current_width == target_width {
            return value;
        }
        let target_ty = self.int_type_for(target_width);
        if current_width < target_width {
            return self.builder.build_int_s_extend(value, target_ty, "int_widen").unwrap();
        }
        let truncated = self.builder.build_int_truncate(value, target_ty, "int_narrow").unwrap();
        let round_tripped = self.builder.build_int_s_extend(truncated, value.get_type(), "int_narrow_roundtrip").unwrap();
        let fits = self.builder.build_int_compare(IntPredicate::EQ, value, round_tripped, "int_narrow_fits").unwrap();
        let overflowed = self.builder.build_not(fits, "int_narrow_overflowed").unwrap();
        self.crash_if(overflowed, &format!("int overflow: value doesn't fit in int[precision:{target_width}]"));
        truncated
    }

    /// Widens both operands of an int binary op to a full i64 --
    /// arithmetic always happens at full width internally (mirroring
    /// `match_float_widths`' "widen to compute" philosophy), so two ints
    /// of different declared widths can be combined directly; the
    /// declared *result* width (computed by the type checker as the
    /// larger of the two operand widths) only matters later, whenever
    /// the result actually gets stored somewhere with `coerce_int_width`.
    fn match_int_widths(&self, li: IntValue<'ctx>, ri: IntValue<'ctx>) -> (IntValue<'ctx>, IntValue<'ctx>) {
        (self.coerce_int_width(li, 64), self.coerce_int_width(ri, 64))
    }

    /// Compiles `expr` as an `int`, trusting the type checker has already
    /// confirmed the whole subtree resolves to `int` -- used specifically
    /// by `compile_and_coerce`'s propagation of a known `int` target into
    /// a `Binary`/`Unary` expression's own operands (see there for why:
    /// `var:int 'c' = (2) xx (10);` has no operand independently anchored
    /// as `int`, so `compile_expr`'s own literal-pairing logic never
    /// triggers on its own). Handles `Expr::Num`/`Binary`/`Unary`
    /// directly and recursively (always at a full i64, narrowed only
    /// once by the caller); anything else (a variable, a call, an array
    /// index) falls back to plain `compile_expr`, which already produces
    /// the correct int-shaped value for those.
    fn compile_int_expr(&mut self, expr: &Expr) -> Result<IntValue<'ctx>, String> {
        match expr {
            Expr::Num(n, text) => Ok(self.context.i64_type().const_int(parse_int_literal(text, *n) as u64, true)),
            Expr::Binary(lhs, op, rhs) if *op != BinOp::Concat => {
                let l = self.compile_int_expr(lhs)?;
                let r = self.compile_int_expr(rhs)?;
                Ok(self.compile_int_binary(*op, l, r).into_int_value())
            }
            Expr::Unary(op, inner) => {
                let iv = self.compile_int_expr(inner)?;
                Ok(match op {
                    UnOp::Neg => self.compile_int_neg(iv),
                    UnOp::Factorial => self.compile_int_factorial(iv),
                    UnOp::Not => panic!("Not on int should have been rejected by the type checker"),
                })
            }
            other => Ok(self.compile_expr(other)?.into_int_value()),
        }
    }

    /// Same role as `compile_int_expr`, for `bigint` -- used by
    /// `compile_and_coerce`'s propagation of a known `bigint` target into
    /// a `Binary`/`Unary` expression's own operands, and by
    /// `compile_expr`'s own `Expr::Binary`/`Unary` arms once they've
    /// detected (via `expr_is_bigint`) that this is a `bigint`
    /// expression -- `bigint` reuses the same `StructValue` *shape*
    /// `bignum`/`array` already use at the LLVM level, so unlike `int`
    /// (whose `IntValue` shape alone already tells it apart from
    /// `bool`/`float`), this can't be discovered from a compiled value's
    /// shape -- the whole subtree has to be compiled through here once
    /// the AST-level check has confirmed it. Returns the raw (unwrapped)
    /// handle; every intermediate handle constructed along the way is
    /// registered in `bigint_temps` for the enclosing statement to free,
    /// exactly like `compile_expr`'s own bignum dispatch does.
    fn compile_bigint_expr(&mut self, expr: &Expr) -> Result<PointerValue<'ctx>, String> {
        match expr {
            Expr::Num(_, text) => {
                let handle = self.bigint_new();
                let text_ptr = self.builder.build_global_string_ptr(text, "bigint_lit").unwrap().as_pointer_value();
                self.builder.build_call(self.bigint.set_str, &[handle.into(), text_ptr.into()], "bigint_set_str_call").unwrap();
                self.bigint_temps.push((handle, self.wrap_bigint_ptr(handle)));
                Ok(handle)
            }
            Expr::Binary(lhs, op, rhs) if *op != BinOp::Concat => {
                let l = self.compile_bigint_expr(lhs)?;
                let r = self.compile_bigint_expr(rhs)?;
                // Only ever reached for an arithmetic op here -- a
                // comparison can't validly appear nested inside a
                // bigint-targeted expression (its result is `bool`, not
                // `bigint`), the type checker already guarantees that.
                // Uses `compile_bigint_arith` directly (the raw handle),
                // not `compile_bigint_binary` -- wrapping the result into
                // a struct only to immediately unwrap it again would
                // create a *second*, different `extractvalue` instruction
                // than the one already pushed into `bigint_temps`
                // (`build_extract_value` mints a fresh instruction every
                // call, even reading the same field of the same struct
                // twice), silently breaking the identity check
                // `compile_and_coerce`'s adoption logic relies on to know
                // which handle it's actually holding.
                Ok(self.compile_bigint_arith(*op, l, r))
            }
            Expr::Unary(op, inner) => {
                let iv = self.compile_bigint_expr(inner)?;
                let dst = self.bigint_new();
                match op {
                    UnOp::Neg => {
                        self.builder.build_call(self.bigint.neg, &[dst.into(), iv.into()], "bigint_neg_call").unwrap();
                    }
                    UnOp::Factorial => {
                        self.builder
                            .build_call(self.bigint.factorial, &[dst.into(), iv.into()], "bigint_factorial_call")
                            .unwrap();
                    }
                    UnOp::Not => panic!("Not on bigint should have been rejected by the type checker"),
                }
                self.bigint_temps.push((dst, self.wrap_bigint_ptr(dst)));
                Ok(dst)
            }
            other => {
                let value = self.compile_expr(other)?;
                Ok(self.unwrap_bigint_ptr(value))
            }
        }
    }

    /// bigint's binary-op entry point -- shared by `compile_expr`'s own
    /// `Expr::Binary` handling (any bigint-involving expression at all,
    /// arithmetic or comparison) and `compile_bigint_expr`'s recursive
    /// arithmetic-only case. `l`/`r` are raw (unwrapped) handles.
    /// Deliberately simple for this first version -- unlike bignum,
    /// there's no chain-fusion/destination-reuse optimization here yet;
    /// every arithmetic op allocates a fresh destination handle.
    fn compile_bigint_binary(&mut self, op: BinOp, l: PointerValue<'ctx>, r: PointerValue<'ctx>) -> BasicValueEnum<'ctx> {
        match op {
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                let cmp = self
                    .builder
                    .build_call(self.bigint.cmp, &[l.into(), r.into()], "bigint_cmp_call")
                    .unwrap()
                    .try_as_basic_value()
                    .basic()
                    .unwrap()
                    .into_int_value();
                let zero = self.context.i32_type().const_int(0, true);
                let predicate = match op {
                    BinOp::Eq => IntPredicate::EQ,
                    BinOp::Ne => IntPredicate::NE,
                    BinOp::Lt => IntPredicate::SLT,
                    BinOp::Gt => IntPredicate::SGT,
                    BinOp::Le => IntPredicate::SLE,
                    BinOp::Ge => IntPredicate::SGE,
                    _ => unreachable!(),
                };
                self.builder.build_int_compare(predicate, cmp, zero, "bigint_cmp").unwrap().into()
            }
            BinOp::And | BinOp::Or => panic!("{op:?} requires bool operands, not bigint"),
            BinOp::Concat => unreachable!("Concat is handled earlier, before this is ever called"),
            _ => {
                let dst = self.compile_bigint_arith(op, l, r);
                self.wrap_bigint_ptr(dst)
            }
        }
    }

    /// The arithmetic-only half of `compile_bigint_binary` (`+ - x / xx
    /// xxx`), returning the *raw* handle rather than the wrapped struct
    /// -- shared with `compile_bigint_expr`'s recursive case, which needs
    /// the raw pointer directly (see its own call site for why going
    /// through the wrapped form there is actively wrong, not just
    /// redundant).
    fn compile_bigint_arith(&mut self, op: BinOp, l: PointerValue<'ctx>, r: PointerValue<'ctx>) -> PointerValue<'ctx> {
        let dst = self.bigint_new();
        match op {
            BinOp::Tetration => {
                self.builder
                    .build_call(self.bigint.tetration, &[dst.into(), l.into(), r.into()], "bigint_tetration_call")
                    .unwrap();
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Pow => {
                let shim_fn = match op {
                    BinOp::Add => self.bigint.add,
                    BinOp::Sub => self.bigint.sub,
                    BinOp::Mul => self.bigint.mul,
                    BinOp::Div => self.bigint.div,
                    BinOp::Pow => self.bigint.pow,
                    _ => unreachable!(),
                };
                self.builder.build_call(shim_fn, &[dst.into(), l.into(), r.into()], "bigint_op_call").unwrap();
            }
            other => panic!("compile_bigint_arith called with non-arithmetic op {other:?}"),
        }
        self.bigint_temps.push((dst, self.wrap_bigint_ptr(dst)));
        dst
    }

    /// Compiles `expr` and coerces it to `ty` -- the `compile_expr` +
    /// `coerce_to_type` pair every storage/passing boundary (variable
    /// declaration, reassignment, function argument, return value) needs.
    /// Special-cased for a direct bare numeric literal assigned to a
    /// `bignum`: `compile_expr` would otherwise produce a lossy `f64`
    /// (`Token::Num` already lost precision beyond ~17 digits at the
    /// lexer stage, same as `num` always has), so the literal's original
    /// text is routed through the same `bignum_set_str` path a
    /// double-quoted string literal already uses instead, preserving
    /// however many digits were actually written. Only a *direct* literal
    /// benefits -- `var:bignum 'x' = 1 + 2;` still goes through `f64` for
    /// the addition itself, same as before.
    fn compile_and_coerce(&mut self, expr: &Expr, ty: Type) -> Result<BasicValueEnum<'ctx>, String> {
        if let (Expr::Num(_, text), Type::BigNum(_)) = (expr, ty) {
            let text_ptr = self.builder.build_global_string_ptr(text, "bignum_lit").unwrap().as_pointer_value();
            return Ok(self.coerce_to_type(text_ptr.into(), ty));
        }
        // An array literal's element type isn't recoverable from the
        // expression alone (an empty `{}` has nothing to infer from) --
        // it only ever comes from a known target type, exactly like a
        // bare bignum literal's precision above. Bypasses compile_expr
        // (whose own ArrayLiteral arm exists purely as a defensive panic)
        // and coerce_to_type entirely -- the freshly built array here is
        // already an independent value, so there's nothing to copy.
        if let (Expr::ArrayLiteral(elements), Type::Array(elem)) = (expr, ty) {
            return self.compile_array_literal(elements, elem);
        }
        // A bare numeric literal assigned to `int` needs a real integer
        // constant, not the f64 `compile_expr`'s generic `Expr::Num` arm
        // always produces. Parses `text` directly rather than going
        // through the already-lossy `n` (an `f64`, exact only up to
        // 2^53) -- the same "read the original digits, not the lexer's
        // lossy float" fix `bignum`'s bare-literal case already needed.
        // Always builds as a full i64 first, then narrows to the target
        // width via `coerce_int_width` (overflow-checked if narrowing) --
        // the same path any other int value goes through when stored,
        // so a too-large literal for a narrow width crashes the same way
        // a too-large *computed* value would, not silently truncated.
        if let (Expr::Num(n, text), Type::Int(width)) = (expr, ty) {
            let i64_val = self.context.i64_type().const_int(parse_int_literal(text, *n) as u64, true);
            return Ok(self.coerce_int_width(i64_val, width).into());
        }
        // Propagate an `int` target into a binary/unary expression's own
        // operands too -- mirrors typecheck.rs's identical propagation
        // exactly (see there for why: `var:int 'c' = (2) xx (10);` has
        // neither operand already known as `int` on its own, so
        // compile_expr's own Binary-arm literal-pairing check -- which
        // only fires when the *other* operand is already int-shaped --
        // would never trigger, and both literals would compile as f64).
        // Delegates the whole subtree to compile_int_expr (which handles
        // Binary/Unary/bare-literal recursively, falling back to
        // compile_expr for anything else, e.g. a variable reference --
        // that must NOT be coerced to the target width here, since a
        // variable's own width might differ and still needs to
        // participate in the arithmetic at its own width first) rather
        // than recursively calling compile_and_coerce on each operand,
        // which would incorrectly narrow a non-literal operand (a wider
        // variable) down to the target width *before* the operation runs.
        if let (Expr::Binary(_, op, _), Type::Int(width)) = (expr, ty) {
            if *op != BinOp::Concat {
                let iv = self.compile_int_expr(expr)?;
                return Ok(self.coerce_int_width(iv, width).into());
            }
        }
        if let (Expr::Unary(_, _), Type::Int(width)) = (expr, ty) {
            let iv = self.compile_int_expr(expr)?;
            return Ok(self.coerce_int_width(iv, width).into());
        }
        // A bare numeric literal assigned to `bigint` -- same reasoning
        // as the `bignum` case above (read the original digit text
        // directly, not the lossy `f64` `compile_expr`'s generic
        // `Expr::Num` arm would produce), just with no precision
        // parameter to thread through.
        if let (Expr::Num(_, text), Type::BigInt) = (expr, ty) {
            let text_ptr = self.builder.build_global_string_ptr(text, "bigint_lit").unwrap().as_pointer_value();
            return Ok(self.coerce_to_type(text_ptr.into(), ty));
        }
        // Same propagation as `int` above -- `var:bigint 'c' = (2) xx
        // (10);` has neither operand independently anchored as `bigint`
        // on its own, so `compile_expr`'s own `Expr::Binary`/`Unary`
        // arms (which only fork into `compile_bigint_expr` once
        // `expr_is_bigint` already finds a `bigint`-shaped operand)
        // would never trigger on their own here.
        if let (Expr::Binary(_, op, _), Type::BigInt) = (expr, ty) {
            if *op != BinOp::Concat {
                let ptr = self.compile_bigint_expr(expr)?;
                // `compile_bigint_expr` always registers its own result in
                // `bigint_temps` (mirroring every other bigint-producing
                // path) -- but this is an early return, bypassing the
                // adoption check further down that would normally remove
                // it. Without this, the handle stays in `bigint_temps`
                // even as it's handed back here as the value actually
                // being stored -- end-of-statement cleanup would then free
                // it right out from under whatever just received it (a
                // real bug, caught by an actual crash/garbage-value test:
                // `var:bigint 'f' = (30)!;` printed a freed handle's
                // leftover bytes instead of the real factorial).
                self.bigint_temps.retain(|(p, _)| *p != ptr);
                return Ok(self.wrap_bigint_ptr(ptr));
            }
        }
        if let (Expr::Unary(_, _), Type::BigInt) = (expr, ty) {
            let ptr = self.compile_bigint_expr(expr)?;
            self.bigint_temps.retain(|(p, _)| *p != ptr);
            return Ok(self.wrap_bigint_ptr(ptr));
        }
        let value = self.compile_expr(expr)?;

        // If `value` is itself a not-yet-consumed bignum_temps entry (a
        // fresh intermediate result from a binary/unary op, or a
        // bignum-returning call) -- nothing else holds a reference to it
        // -- and its actual precision matches the target exactly, storing
        // it doesn't need a real copy at all: adopt the existing handle
        // directly instead of allocating a new one and running
        // bignum_set_d/set_str/copy just to duplicate data that's about
        // to be thrown away anyway. Removed from bignum_temps so the
        // end-of-statement drain doesn't free out from under whatever now
        // owns it. Skipped when the precision doesn't match (most
        // commonly: storing a call's bignum result into a variable
        // declared at a different precision) -- that genuinely needs a
        // real copy/conversion, same as before.
        if let Type::BigNum(target_precision) = ty {
            if let Some(idx) = self.bignum_temps.iter().position(|(_, v, p)| *v == value && *p == target_precision) {
                self.bignum_temps.remove(idx);
                return Ok(value);
            }
        }

        // Same adoption trick as bignum's above, simpler here since
        // there's no precision to match -- just whether this exact value
        // is a not-yet-consumed bigint_temps entry.
        if ty == Type::BigInt {
            if let Some(idx) = self.bigint_temps.iter().position(|(_, v)| *v == value) {
                self.bigint_temps.remove(idx);
                return Ok(value);
            }
        }

        // Same reasoning as bignum's adoption check above, simpler here
        // since a `str`/`file` value has no precision to match -- just the
        // raw pointer. If `value` is itself a not-yet-consumed `str_temps`
        // entry (a `stch` result or a str-returning call) -- nothing else
        // holds a reference to it -- adopt it directly instead of running
        // a redundant `strdup` just to duplicate a buffer that's about to
        // be thrown away anyway. A plain string literal's rodata pointer
        // and a variable's own already-owned buffer are never pushed to
        // `str_temps`, so this can never wrongly adopt something that
        // still genuinely needs its own copy (a literal) or that another
        // variable still owns (a plain read).
        if matches!(ty, Type::Str | Type::File) {
            if let BasicValueEnum::PointerValue(p) = value {
                if let Some(idx) = self.str_temps.iter().position(|&v| v == p) {
                    self.str_temps.remove(idx);
                    return Ok(value);
                }
            }
        }

        Ok(self.coerce_to_type(value, ty))
    }

    /// Compiles a bare numeric literal (`text`/`n`) that's paired, in a
    /// binary op, with an already-compiled `other` operand -- built and
    /// overflow-checked directly at `other`'s own width (parsed from
    /// `text` as a full i64 first, same precision reasoning as the
    /// bare-literal case in `compile_and_coerce`, then narrowed via
    /// `coerce_int_width`) if `other` is genuinely `int` (any of its
    /// widths -- an `IntValue` that isn't `bool`'s i1), otherwise as the
    /// usual f64. Matching `other`'s actual width here (rather than
    /// always building a full i64) matters for `compile_int_binary`'s own
    /// result-width computation right after this returns -- it needs the
    /// literal's *real* width, or a literal paired with a narrow variable
    /// would look artificially 64-bit-wide and never get narrowed back
    /// down for its own overflow check. The type checker has already
    /// confirmed the literal is a whole number whenever this produces
    /// `int`.
    fn compile_literal_paired_with(&self, text: &str, n: f64, other: &BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        if let BasicValueEnum::IntValue(iv) = other {
            if iv.get_type().get_bit_width() != 1 {
                let i64_val = self.context.i64_type().const_int(parse_int_literal(text, n) as u64, true);
                return self.coerce_int_width(i64_val, iv.get_type().get_bit_width()).into();
            }
        }
        self.context.f64_type().const_float(n).into()
    }

    /// Same job as `compile_literal_paired_with`, except when `other_expr`
    /// is bignum-shaped and this exact literal was already hoisted out of
    /// the enclosing loop (`Stmt::While`'s codegen, via
    /// `find_hoistable_bignum_literals`): reuses that already-constructed
    /// handle directly (a plain variable load) instead of paying for a
    /// fresh `bignum_new` + `set_d` again. Checked from the innermost
    /// active loop outward, though in practice a given literal occurrence
    /// only ever has a hoisted entry in exactly one active frame -- each
    /// loop's scan never descends into a *nested* loop's own body, so
    /// there's no ambiguity about which frame it belongs to. Falls back to
    /// the exact old behavior whenever nothing is hoisted for it (outside
    /// any loop, or `other_expr` isn't bignum-shaped at all).
    fn compile_hoisted_or_literal(
        &self,
        text: &str,
        n: f64,
        other_expr: &Expr,
        other_value: &BasicValueEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        if let Some(precision) = self.bignum_precision_of_expr(other_expr) {
            let bits = n.to_bits();
            for frame in self.hoisted_bignum_literals.iter().rev() {
                if let Some(key) = frame.get(&(bits, precision)) {
                    let (ptr, llvm_ty) = self.variables[key];
                    return self.builder.build_load(llvm_ty, ptr, "hoisted_bignum_lit_load").unwrap();
                }
            }
        }
        self.compile_literal_paired_with(text, n, other_value)
    }

    /// Builds a fresh array from `{(v1), (v2), ...}`, appending each
    /// element (itself run through `compile_and_coerce` against `elem`,
    /// so e.g. a bare bignum literal inside an array of `bignum` still
    /// gets its full precision, not just an `f64`'s worth).
    fn compile_array_literal(&mut self, elements: &[Expr], elem: ElementType) -> Result<BasicValueEnum<'ctx>, String> {
        let elem_ty = elem.as_type();
        let elem_llvm_ty = self.basic_type(elem_ty);
        let elem_size = elem_llvm_ty.size_of().expect("every element type has a known size");
        let handle = self
            .builder
            .build_call(self.array_new_fn, &[elem_size.into()], "array_lit_new")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();

        for elem_expr in elements {
            let value = self.compile_and_coerce(elem_expr, elem_ty)?;
            let value_slot = self.entry_alloca(elem_llvm_ty, "array_lit_value_slot");
            self.builder.build_store(value_slot, value).unwrap();
            self.builder
                .build_call(self.array_append_fn, &[handle.into(), value_slot.into()], "array_lit_append_call")
                .unwrap();
        }

        Ok(self.wrap_bignum_ptr(handle))
    }

    /// Converts an already-compiled array value into a *freshly allocated,
    /// independent copy* -- the same "always copy on store" rule
    /// `bignum`/`str` already follow, and for arrays specifically also a
    /// correctness requirement: without an independent copy, two
    /// variables could end up sharing the same handle, and each being
    /// freed independently at its own scope exit would double-free it.
    /// Deep-copies `str`/`file`/`bignum` elements too (each is
    /// independently heap-owned); `num`/`numw`/`bool` elements are copied
    /// by value as-is.
    fn coerce_to_array(&self, value: BasicValueEnum<'ctx>, elem: ElementType) -> BasicValueEnum<'ctx> {
        let function = self.current_function();
        let src_handle = self.unwrap_bignum_ptr(value);
        let elem_ty = elem.as_type();
        let elem_llvm_ty = self.basic_type(elem_ty);
        let elem_size = elem_llvm_ty.size_of().expect("every element type has a known size");

        let new_handle = self
            .builder
            .build_call(self.array_new_fn, &[elem_size.into()], "array_copy_new")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();

        let length = self
            .builder
            .build_call(self.array_length_fn, &[src_handle.into()], "array_copy_len")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_int_value();

        let i64_ty = self.context.i64_type();
        let counter_slot = self.entry_alloca(i64_ty.into(), "array_copy_i");
        self.builder.build_store(counter_slot, i64_ty.const_int(1, true)).unwrap();

        let cond_bb = self.context.append_basic_block(function, "array_copy_cond");
        let body_bb = self.context.append_basic_block(function, "array_copy_body");
        let end_bb = self.context.append_basic_block(function, "array_copy_end");
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(cond_bb);
        let counter = self.builder.build_load(i64_ty, counter_slot, "array_copy_i_load").unwrap().into_int_value();
        let keep_going = self
            .builder
            .build_int_compare(IntPredicate::SLE, counter, length, "array_copy_test")
            .unwrap();
        self.builder.build_conditional_branch(keep_going, body_bb, end_bb).unwrap();

        self.builder.position_at_end(body_bb);
        let src_slot = self
            .builder
            .build_call(self.array_get_ptr_fn, &[src_handle.into(), counter.into()], "array_copy_src_slot")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();
        let loaded = self.builder.build_load(elem_llvm_ty, src_slot, "array_copy_elem").unwrap();
        let to_append = match elem {
            ElementType::Str | ElementType::File => self
                .builder
                .build_call(self.strdup_fn, &[loaded.into_pointer_value().into()], "array_copy_elem_strdup")
                .unwrap()
                .try_as_basic_value()
                .basic()
                .unwrap(),
            ElementType::BigNum(precision) => self.coerce_to_bignum(loaded, precision),
            ElementType::BigInt => self.coerce_to_bigint(loaded),
            ElementType::Num(_) | ElementType::NumW(_) | ElementType::Bool | ElementType::Int(_) => loaded,
        };
        let value_slot = self.entry_alloca(elem_llvm_ty, "array_copy_value_slot");
        self.builder.build_store(value_slot, to_append).unwrap();
        self.builder
            .build_call(self.array_append_fn, &[new_handle.into(), value_slot.into()], "array_copy_append_call")
            .unwrap();
        let counter_next = self.builder.build_int_add(counter, i64_ty.const_int(1, true), "array_copy_i_next").unwrap();
        self.builder.build_store(counter_slot, counter_next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(end_bb);
        self.wrap_bignum_ptr(new_handle)
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
        if self.declared_in_current_scope(&key) {
            return;
        }
        let entry = match self.variables.get(&key) {
            Some(&old) => ScopeEntry::Shadowed(key, old),
            None => ScopeEntry::New(key),
        };
        self.scopes.last_mut().unwrap().push(entry);
    }

    /// Whether `key` was already declared in the *current* (innermost)
    /// block -- as opposed to merely existing via an outer block's
    /// binding, which `declare_scoped` shadows rather than reuses. Only a
    /// true same-block redeclaration should free its old value before
    /// being replaced; freeing on a fresh shadow would destroy a value the
    /// outer block still owns and expects to find intact once this block
    /// ends (a real bug this fixed: shadowing a `bignum`/`str` name used
    /// to free the *outer* variable immediately, leaving it dangling for
    /// the rest of the outer block and double-freed at its own scope
    /// exit).
    fn declared_in_current_scope(&self, key: &(String, Type)) -> bool {
        self.scopes
            .last()
            .expect("declared_in_current_scope called outside any block")
            .iter()
            .any(|e| e.key() == key)
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

    /// Same job as `free_bignum_var`, for `bigint`.
    fn free_bigint_var(&mut self, key: &(String, Type)) {
        let (ptr, llvm_ty) = *self.variables.get(key).expect("free_bigint_var on unknown variable");
        let loaded = self.builder.build_load(llvm_ty, ptr, "bigint_for_free").unwrap();
        let handle = self.unwrap_bigint_ptr(loaded);
        self.free_bigint_ptr(handle);
    }

    fn free_bigint_ptr(&mut self, ptr: PointerValue<'ctx>) {
        self.builder.build_call(self.bigint.free, &[ptr.into()], "bigint_free_call").unwrap();
    }

    /// Frees the `str` buffer currently stored in `key`'s variable slot.
    /// Always safe to call unconditionally: every `str` variable's stored
    /// pointer is always its own `strdup`'d copy (see `coerce_to_type`),
    /// never a bare literal's rodata pointer.
    fn free_str_var(&mut self, key: &(String, Type)) {
        let (ptr, llvm_ty) = *self.variables.get(key).expect("free_str_var on unknown variable");
        let loaded = self.builder.build_load(llvm_ty, ptr, "str_for_free").unwrap().into_pointer_value();
        self.builder.build_call(self.libc_free, &[loaded.into()], "str_free_call").unwrap();
    }

    /// Frees the array currently stored in `key`'s variable slot -- reads
    /// the element type straight off `key` itself, since `Type::Array`
    /// carries it.
    /// Determines the element type of an expression already known to
    /// evaluate to `Type::Array(_)`. In practice this is always a direct
    /// `ref:var:array:TYPE` reference -- there's no other way yet to
    /// produce a *named, appendable* array value (no array-returning
    /// functions, and a fresh literal has nothing to append into).
    fn array_element_type_of(expr: &Expr) -> ElementType {
        match expr {
            Expr::Var(_, Type::Array(elem)) => *elem,
            other => panic!("expected an array variable reference, found {other:?} -- should have been caught by the type checker"),
        }
    }

    fn free_array_var(&mut self, key: &(String, Type)) {
        let elem = match key.1 {
            Type::Array(elem) => elem,
            other => panic!("free_array_var called on non-array key: {other:?}"),
        };
        let (ptr, llvm_ty) = *self.variables.get(key).expect("free_array_var on unknown variable");
        let loaded = self.builder.build_load(llvm_ty, ptr, "array_for_free").unwrap();
        let handle = self.unwrap_bignum_ptr(loaded);
        self.free_array_ptr(handle, elem);
    }

    /// Frees an array's own handle+buffer. For `Str`/`File`/`BigNum`
    /// elements (each independently heap-owned), every element is freed
    /// first via a real runtime loop (the length is only known at
    /// runtime) -- `Num`/`NumW`/`Bool` elements need no per-element
    /// cleanup at all, since they're plain fixed-size values with nothing
    /// of their own to free.
    fn free_array_ptr(&mut self, handle: PointerValue<'ctx>, elem: ElementType) {
        match elem {
            ElementType::Str | ElementType::File => self.free_array_str_elements(handle),
            ElementType::BigNum(_) => self.free_array_bignum_elements(handle),
            ElementType::BigInt => self.free_array_bigint_elements(handle),
            ElementType::Num(_) | ElementType::NumW(_) | ElementType::Bool | ElementType::Int(_) => {}
        }
        self.builder.build_call(self.array_free_fn, &[handle.into()], "array_free_call").unwrap();
    }

    /// Loops from 1 to the array's length, `libc_free`-ing each `str`/
    /// `file` element's own buffer. Mirrors `compile_tetration`'s loop
    /// shape (a real runtime loop, since the length is only known then).
    fn free_array_str_elements(&mut self, handle: PointerValue<'ctx>) {
        let function = self.current_function();
        let i64_ty = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        let length = self
            .builder
            .build_call(self.array_length_fn, &[handle.into()], "arr_free_len")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_int_value();

        let counter_slot = self.entry_alloca(i64_ty.into(), "arr_free_i");
        self.builder.build_store(counter_slot, i64_ty.const_int(1, true)).unwrap();

        let cond_bb = self.context.append_basic_block(function, "arr_free_str_cond");
        let body_bb = self.context.append_basic_block(function, "arr_free_str_body");
        let end_bb = self.context.append_basic_block(function, "arr_free_str_end");
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(cond_bb);
        let counter = self.builder.build_load(i64_ty, counter_slot, "arr_free_str_i_load").unwrap().into_int_value();
        let keep_going = self
            .builder
            .build_int_compare(IntPredicate::SLE, counter, length, "arr_free_str_test")
            .unwrap();
        self.builder.build_conditional_branch(keep_going, body_bb, end_bb).unwrap();

        self.builder.position_at_end(body_bb);
        let slot_ptr = self
            .builder
            .build_call(self.array_get_ptr_fn, &[handle.into(), counter.into()], "arr_free_str_slot")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();
        let elem_ptr = self.builder.build_load(ptr_ty, slot_ptr, "arr_free_str_elem").unwrap().into_pointer_value();
        self.builder.build_call(self.libc_free, &[elem_ptr.into()], "arr_free_str_elem_call").unwrap();
        let counter_next = self.builder.build_int_add(counter, i64_ty.const_int(1, true), "arr_free_str_i_next").unwrap();
        self.builder.build_store(counter_slot, counter_next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(end_bb);
    }

    /// Same shape as `free_array_str_elements`, but for `bignum` elements
    /// -- each slot holds the same wrapped-pointer struct any other
    /// `bignum` value does, freed via `bignum.free` after unwrapping.
    fn free_array_bignum_elements(&mut self, handle: PointerValue<'ctx>) {
        let function = self.current_function();
        let i64_ty = self.context.i64_type();
        let bignum_ty = self.bignum_struct_type();

        let length = self
            .builder
            .build_call(self.array_length_fn, &[handle.into()], "arr_free_bignum_len")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_int_value();

        let counter_slot = self.entry_alloca(i64_ty.into(), "arr_free_bignum_i");
        self.builder.build_store(counter_slot, i64_ty.const_int(1, true)).unwrap();

        let cond_bb = self.context.append_basic_block(function, "arr_free_bignum_cond");
        let body_bb = self.context.append_basic_block(function, "arr_free_bignum_body");
        let end_bb = self.context.append_basic_block(function, "arr_free_bignum_end");
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(cond_bb);
        let counter = self.builder.build_load(i64_ty, counter_slot, "arr_free_bignum_i_load").unwrap().into_int_value();
        let keep_going = self
            .builder
            .build_int_compare(IntPredicate::SLE, counter, length, "arr_free_bignum_test")
            .unwrap();
        self.builder.build_conditional_branch(keep_going, body_bb, end_bb).unwrap();

        self.builder.position_at_end(body_bb);
        let slot_ptr = self
            .builder
            .build_call(self.array_get_ptr_fn, &[handle.into(), counter.into()], "arr_free_bignum_slot")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();
        let elem_wrapped = self.builder.build_load(bignum_ty, slot_ptr, "arr_free_bignum_elem").unwrap();
        let elem_ptr = self.unwrap_bignum_ptr(elem_wrapped);
        self.free_bignum_ptr(elem_ptr);
        let counter_next = self.builder.build_int_add(counter, i64_ty.const_int(1, true), "arr_free_bignum_i_next").unwrap();
        self.builder.build_store(counter_slot, counter_next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(end_bb);
    }

    /// Same shape as `free_array_bignum_elements`, for `bigint` elements.
    fn free_array_bigint_elements(&mut self, handle: PointerValue<'ctx>) {
        let function = self.current_function();
        let i64_ty = self.context.i64_type();
        let bigint_ty = self.bigint_struct_ty;

        let length = self
            .builder
            .build_call(self.array_length_fn, &[handle.into()], "arr_free_bigint_len")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_int_value();

        let counter_slot = self.entry_alloca(i64_ty.into(), "arr_free_bigint_i");
        self.builder.build_store(counter_slot, i64_ty.const_int(1, true)).unwrap();

        let cond_bb = self.context.append_basic_block(function, "arr_free_bigint_cond");
        let body_bb = self.context.append_basic_block(function, "arr_free_bigint_body");
        let end_bb = self.context.append_basic_block(function, "arr_free_bigint_end");
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(cond_bb);
        let counter = self.builder.build_load(i64_ty, counter_slot, "arr_free_bigint_i_load").unwrap().into_int_value();
        let keep_going = self
            .builder
            .build_int_compare(IntPredicate::SLE, counter, length, "arr_free_bigint_test")
            .unwrap();
        self.builder.build_conditional_branch(keep_going, body_bb, end_bb).unwrap();

        self.builder.position_at_end(body_bb);
        let slot_ptr = self
            .builder
            .build_call(self.array_get_ptr_fn, &[handle.into(), counter.into()], "arr_free_bigint_slot")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();
        let elem_wrapped = self.builder.build_load(bigint_ty, slot_ptr, "arr_free_bigint_elem").unwrap();
        let elem_ptr = self.unwrap_bigint_ptr(elem_wrapped);
        self.free_bigint_ptr(elem_ptr);
        let counter_next = self.builder.build_int_add(counter, i64_ty.const_int(1, true), "arr_free_bigint_i_next").unwrap();
        self.builder.build_store(counter_slot, counter_next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(end_bb);
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
                match entry.key() {
                    (_, Type::BigNum(_)) => self.free_bignum_var(entry.key()),
                    (_, Type::BigInt) => self.free_bigint_var(entry.key()),
                    (_, Type::Str | Type::File) => self.free_str_var(entry.key()),
                    (_, Type::Array(_)) => self.free_array_var(entry.key()),
                    _ => {}
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
            Type::NumW(width) => self.float_type_for(width).fn_type(&param_types, false),
            Type::Bool => self.context.bool_type().fn_type(&param_types, false),
            Type::Int(width) => self.int_type_for(width).fn_type(&param_types, false),
            Type::Str | Type::File => self.context.ptr_type(AddressSpace::default()).fn_type(&param_types, false),
            Type::BigNum(_) | Type::Array(_) => self.bignum_struct_type().fn_type(&param_types, false),
            Type::BigInt => self.bigint_struct_ty.fn_type(&param_types, false),
            Type::Void => self.context.void_type().fn_type(&param_types, false),
        };

        let function = self.module.add_function(&f.name, fn_type, None);
        self.functions.insert(f.name.clone(), function);
        let param_sig_types: Vec<Type> = f.params.iter().map(|p| p.ty).collect();
        self.function_sigs.insert(f.name.clone(), (param_sig_types, f.return_type));
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
        self.current_return_type = Type::Void;
        self.in_entry = true;

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
        self.current_return_type = f.return_type;
        self.in_entry = false;
        // Wraps params + the whole body in one scope, so a bignum
        // parameter gets freed exactly like any other bignum local --
        // whether via an explicit `return` (whose own scope-walk already
        // reaches every open frame, params' included) or by falling off
        // the end below.
        self.scopes.push(Vec::new());

        for (i, param) in f.params.iter().enumerate() {
            let value = function.get_nth_param(i as u32).unwrap();
            let ty = self.basic_type(param.ty);
            let alloca = self.builder.build_alloca(ty, &param.name).unwrap();
            self.builder.build_store(alloca, value).unwrap();
            let key = (param.name.clone(), param.ty);
            self.declare_scoped(key.clone());
            self.variables.insert(key, (alloca, ty));
        }

        self.compile_block(&f.body)?;

        // Every LLVM basic block must end in a terminator. If the source
        // fell off the end of the function without an explicit `return`,
        // patch one in (a real type checker would flag this as missing
        // a return on some path instead of silently defaulting).
        let current_block = self.builder.get_insert_block().unwrap();
        let terminated = current_block.get_terminator().is_some();
        // Pop the param scope *before* building any default-return
        // terminator below: if not terminated, this is where bignum
        // params actually get freed, and it has to happen before the
        // terminator since nothing can follow one in the same block. If
        // already terminated, an explicit `return` already walked this
        // same scope and freed it -- this call only does the (silent)
        // variable-table bookkeeping, no duplicate frees.
        self.pop_scope(!terminated);
        if !terminated {
            match f.return_type {
                Type::Void => {
                    self.builder.build_return(None).unwrap();
                }
                Type::Num(width) => {
                    let zero = self.float_type_for(width).const_float(0.0);
                    self.builder.build_return(Some(&zero)).unwrap();
                }
                Type::NumW(width) => {
                    let zero = self.float_type_for(width).const_float(0.0);
                    self.builder.build_return(Some(&zero)).unwrap();
                }
                Type::Bool => {
                    let zero = self.context.bool_type().const_int(0, false);
                    self.builder.build_return(Some(&zero)).unwrap();
                }
                Type::Int(width) => {
                    let zero = self.int_type_for(width).const_int(0, true);
                    self.builder.build_return(Some(&zero)).unwrap();
                }
                Type::Str | Type::File => {
                    let null = self.context.ptr_type(AddressSpace::default()).const_null();
                    self.builder.build_return(Some(&null)).unwrap();
                }
                Type::BigNum(_) | Type::Array(_) => {
                    let null_ptr = self.context.ptr_type(AddressSpace::default()).const_null();
                    let zero = self.wrap_bignum_ptr(null_ptr);
                    self.builder.build_return(Some(&zero)).unwrap();
                }
                Type::BigInt => {
                    let null_ptr = self.context.ptr_type(AddressSpace::default()).const_null();
                    let zero = self.wrap_bigint_ptr(null_ptr);
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
                let value = self.compile_and_coerce(expr, *ty)?;
                let key = (name.clone(), *ty);

                // Re-declaring a bignum/str name already declared in *this
                // exact block* would otherwise leak its old handle -- free
                // it before the slot's replaced. Must NOT fire when this is
                // actually a fresh shadow of an outer block's variable
                // (checked via declared_in_current_scope, not just
                // self.variables) -- the outer one is still alive and
                // owned by its own block.
                if self.declared_in_current_scope(&key) {
                    match *ty {
                        Type::BigNum(_) => self.free_bignum_var(&key),
                        Type::BigInt => self.free_bigint_var(&key),
                        Type::Str | Type::File => self.free_str_var(&key),
                        Type::Array(_) => self.free_array_var(&key),
                        _ => {}
                    }
                }

                let llvm_ty = self.basic_type(*ty);
                let alloca = self.entry_alloca(llvm_ty, name);
                self.builder.build_store(alloca, value).unwrap();
                self.declare_scoped(key.clone());
                self.variables.insert(key, (alloca, llvm_ty));
            }
            Stmt::Input(name, ty, source) => {
                let key = (name.clone(), *ty);

                // Same redeclare-vs-shadow distinction as VarDecl above.
                if self.declared_in_current_scope(&key) {
                    match *ty {
                        Type::BigNum(_) => self.free_bignum_var(&key),
                        Type::BigInt => self.free_bigint_var(&key),
                        Type::Str | Type::File => self.free_str_var(&key),
                        Type::Array(_) => self.free_array_var(&key),
                        _ => {}
                    }
                }

                let value: BasicValueEnum = match (*ty, source) {
                    // cyborg_read_line's/cyborg_read_file_or_die's result
                    // is already a fresh, owned malloc'd buffer -- adopted
                    // directly, no strdup needed either way.
                    (Type::Str, None) => self
                        .builder
                        .build_call(self.read_line_fn, &[], "read_line_call")
                        .unwrap()
                        .try_as_basic_value()
                        .basic()
                        .unwrap(),
                    (Type::Str, Some(src)) => {
                        let path = self.compile_expr(src)?.into_pointer_value();
                        self.builder
                            .build_call(self.read_file_fn, &[path.into()], "read_file_call")
                            .unwrap()
                            .try_as_basic_value()
                            .basic()
                            .unwrap()
                    }
                    (Type::Num(width), None) => {
                        let raw = self
                            .builder
                            .build_call(self.read_num_fn, &[], "read_num_call")
                            .unwrap()
                            .try_as_basic_value()
                            .basic()
                            .unwrap()
                            .into_float_value();
                        self.coerce_float(raw, width).into()
                    }
                    (Type::Num(width), Some(src)) => {
                        let path = self.compile_expr(src)?.into_pointer_value();
                        let text = self
                            .builder
                            .build_call(self.read_file_fn, &[path.into()], "read_file_call")
                            .unwrap()
                            .try_as_basic_value()
                            .basic()
                            .unwrap()
                            .into_pointer_value();
                        let raw = self
                            .builder
                            .build_call(self.parse_num_or_die_fn, &[text.into()], "parse_num_call")
                            .unwrap()
                            .try_as_basic_value()
                            .basic()
                            .unwrap()
                            .into_float_value();
                        // Unlike the Str case, nothing adopts this buffer
                        // -- it was only needed to extract the number, so
                        // it must be freed once consumed.
                        self.builder.build_call(self.libc_free, &[text.into()], "read_file_temp_free_call").unwrap();
                        self.coerce_float(raw, width).into()
                    }
                    (other, _) => panic!("input not supported for {other:?} yet"),
                };

                let llvm_ty = self.basic_type(*ty);
                let alloca = self.entry_alloca(llvm_ty, name);
                self.builder.build_store(alloca, value).unwrap();
                self.declare_scoped(key.clone());
                self.variables.insert(key, (alloca, llvm_ty));
            }
            Stmt::Clock(name, ty) => {
                let key = (name.clone(), *ty);

                // Same redeclare-vs-shadow distinction as VarDecl/Input
                // above -- Num never needs freeing, but kept for
                // consistency if this ever grows more types.
                if self.declared_in_current_scope(&key) {
                    match *ty {
                        Type::BigNum(_) => self.free_bignum_var(&key),
                        Type::BigInt => self.free_bigint_var(&key),
                        Type::Str | Type::File => self.free_str_var(&key),
                        Type::Array(_) => self.free_array_var(&key),
                        _ => {}
                    }
                }

                let value: BasicValueEnum = match *ty {
                    Type::Num(width) => {
                        let raw = self
                            .builder
                            .build_call(self.clock_elapsed_fn, &[], "clock_elapsed_call")
                            .unwrap()
                            .try_as_basic_value()
                            .basic()
                            .unwrap()
                            .into_float_value();
                        self.coerce_float(raw, width).into()
                    }
                    other => panic!("clock not supported for {other:?} yet"),
                };

                let llvm_ty = self.basic_type(*ty);
                let alloca = self.entry_alloca(llvm_ty, name);
                self.builder.build_store(alloca, value).unwrap();
                self.declare_scoped(key.clone());
                self.variables.insert(key, (alloca, llvm_ty));
            }
            Stmt::Assign(name, ty, expr) => {
                let value = self.compile_and_coerce(expr, *ty)?;
                let key = (name.clone(), *ty);
                let (ptr, _ty) = *self
                    .variables
                    .get(&key)
                    .ok_or_else(|| format!("undefined variable '{name}' of type {ty:?}"))?;
                // Reassignment always stores a fresh handle (coerce_to_type
                // -> coerce_to_bignum/strdup), so the old one must be freed
                // here or it leaks -- the slot itself doesn't change, only
                // what it points at.
                match *ty {
                    Type::BigNum(_) => self.free_bignum_var(&key),
                    Type::BigInt => self.free_bigint_var(&key),
                    Type::Str | Type::File => self.free_str_var(&key),
                    Type::Array(_) => self.free_array_var(&key),
                    _ => {}
                }
                self.builder.build_store(ptr, value).unwrap();
            }
            Stmt::ArrayIndexAssign(name, ty, index, value_expr) => {
                let elem = match *ty {
                    Type::Array(elem) => elem,
                    other => panic!("ArrayIndexAssign on non-array type {other:?} -- should have been caught by the type checker"),
                };
                let key = (name.clone(), *ty);
                let (ptr, llvm_ty) = *self
                    .variables
                    .get(&key)
                    .ok_or_else(|| format!("undefined variable '{name}' of type {ty:?}"))?;
                let array_value = self.builder.build_load(llvm_ty, ptr, name).unwrap();
                let handle = self.unwrap_bignum_ptr(array_value);
                let index_value = self.compile_expr(index)?.into_float_value();
                let index_i64 = self
                    .builder
                    .build_float_to_signed_int(index_value, self.context.i64_type(), "array_assign_index_i64")
                    .unwrap();
                let slot_ptr = self
                    .builder
                    .build_call(self.array_get_ptr_fn, &[handle.into(), index_i64.into()], "array_assign_get_ptr")
                    .unwrap()
                    .try_as_basic_value()
                    .basic()
                    .unwrap()
                    .into_pointer_value();

                let elem_ty = elem.as_type();
                let new_value = self.compile_and_coerce(value_expr, elem_ty)?;

                // The new value is always an independent copy (same
                // "always copy on store" rule as everywhere else), so it's
                // safe to free whatever the slot held *before* storing the
                // new value over it.
                let elem_llvm_ty = self.basic_type(elem_ty);
                match elem {
                    ElementType::Str | ElementType::File => {
                        let old_ptr = self.builder.build_load(elem_llvm_ty, slot_ptr, "array_assign_old").unwrap();
                        self.builder.build_call(self.libc_free, &[old_ptr.into()], "array_assign_old_free_call").unwrap();
                    }
                    ElementType::BigNum(_) => {
                        let old_wrapped = self.builder.build_load(elem_llvm_ty, slot_ptr, "array_assign_old").unwrap();
                        let old_ptr = self.unwrap_bignum_ptr(old_wrapped);
                        self.free_bignum_ptr(old_ptr);
                    }
                    ElementType::BigInt => {
                        let old_wrapped = self.builder.build_load(elem_llvm_ty, slot_ptr, "array_assign_old").unwrap();
                        let old_ptr = self.unwrap_bigint_ptr(old_wrapped);
                        self.free_bigint_ptr(old_ptr);
                    }
                    ElementType::Num(_) | ElementType::NumW(_) | ElementType::Bool | ElementType::Int(_) => {}
                }

                self.builder.build_store(slot_ptr, new_value).unwrap();
            }
            Stmt::Append(array_expr, value_expr) => {
                let elem = Self::array_element_type_of(array_expr);
                let array_value = self.compile_expr(array_expr)?;
                let handle = self.unwrap_bignum_ptr(array_value);

                let elem_ty = elem.as_type();
                let value = self.compile_and_coerce(value_expr, elem_ty)?;
                let elem_llvm_ty = self.basic_type(elem_ty);
                let value_slot = self.entry_alloca(elem_llvm_ty, "append_value_slot");
                self.builder.build_store(value_slot, value).unwrap();
                self.builder
                    .build_call(self.array_append_fn, &[handle.into(), value_slot.into()], "append_call")
                    .unwrap();
            }
            Stmt::Return(expr) => {
                // Coerced (via compile_and_coerce, so a bare bignum literal
                // still gets its full precision) to the function's
                // declared return type *before* anything below gets freed.
                // For bignum this either makes an independent copy or (if
                // the source was itself a not-yet-consumed bignum_temps
                // entry at the exact right precision) adopts that handle
                // directly, removing it from bignum_temps -- either way,
                // whatever the source was (a named variable, a computed
                // intermediate) is safe to handle unconditionally
                // afterward: a named variable's value was never aliased by
                // the copy, and an adopted temp is already gone from
                // bignum_temps by the time the drain below runs, so it's
                // not freed out from under the value being returned. This
                // also closes the old Expr::Call bignum leak: a caller now
                // always receives its own handle, never one it has to
                // guess whether it owns.
                let return_ty = self.current_return_type;
                let coerced = match expr {
                    Some(e) => Some(self.compile_and_coerce(e, return_ty)?),
                    None => None,
                };

                let temps: Vec<(PointerValue<'ctx>, BasicValueEnum<'ctx>, u32)> = self.bignum_temps.drain(..).collect();
                for (ptr, _, _) in temps {
                    self.free_bignum_ptr(ptr);
                }
                let bigint_temps: Vec<(PointerValue<'ctx>, BasicValueEnum<'ctx>)> = self.bigint_temps.drain(..).collect();
                for (ptr, _) in bigint_temps {
                    self.free_bigint_ptr(ptr);
                }
                // Same reasoning as bignum_temps above: coerce_to_type
                // always strdup's a str return value, so any str_temps
                // registered while evaluating it (a stch result, a nested
                // call's own str return) are safely superseded and can be
                // freed unconditionally here too.
                let str_temps: Vec<PointerValue<'ctx>> = self.str_temps.drain(..).collect();
                for ptr in str_temps {
                    self.builder.build_call(self.libc_free, &[ptr.into()], "str_temp_free_call").unwrap();
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
                    .filter(|key| matches!(key.1, Type::BigNum(_) | Type::Str | Type::File | Type::Array(_) | Type::BigInt))
                    .cloned()
                    .collect();
                for key in &to_free {
                    match key.1 {
                        Type::BigNum(_) => self.free_bignum_var(key),
                        Type::BigInt => self.free_bigint_var(key),
                        Type::Str | Type::File => self.free_str_var(key),
                        Type::Array(_) => self.free_array_var(key),
                        _ => unreachable!(),
                    }
                }
                match coerced {
                    Some(value) => {
                        self.builder.build_return(Some(&value)).unwrap();
                    }
                    // A bare `return;` inside `START...END` still has to
                    // satisfy `main`'s real LLVM signature (`i32`, the C
                    // runtime's own convention) even though there's no
                    // source-level return type to coerce against here --
                    // `ret void` there would leave main's return-value
                    // register unset, an undefined process exit code
                    // rather than the intended "ran fine" 0. Matches the
                    // same `ret i32 0` the fall-off-the-end case already
                    // builds in `compile_entry`. A real void-returning
                    // user function still gets a genuine `ret void` here,
                    // unaffected.
                    None if self.in_entry => {
                        let zero = self.context.i32_type().const_int(0, false);
                        self.builder.build_return(Some(&zero)).unwrap();
                    }
                    None => {
                        self.builder.build_return(None).unwrap();
                    }
                };
            }
            Stmt::Print(segments, dest) => {
                let (fmt, args, to_free) = self.compile_print_segments(segments)?;
                let fmt_global = self.builder.build_global_string_ptr(&fmt, "fmt").unwrap();
                let mut call_args: Vec<BasicMetadataValueEnum> = vec![fmt_global.as_pointer_value().into()];
                call_args.extend(args);

                match dest {
                    None => {
                        self.builder.build_call(self.printf_fn, &call_args, "printf_call").unwrap();
                    }
                    Some(dest_expr) => {
                        self.compile_write_to_file(dest_expr, &call_args)?;
                    }
                }

                // Only after the write has actually consumed them: each
                // bignum's formatted string is a fresh malloc'd buffer
                // (GMP's default allocator) that nothing else references
                // once this call is done with it.
                for ptr in to_free {
                    self.builder.build_call(self.libc_free, &[ptr.into()], "bignum_fmt_free_call").unwrap();
                }
            }
            Stmt::Overwrite(segments, dest) => {
                let (fmt, args, to_free) = self.compile_print_segments(segments)?;
                let fmt_global = self.builder.build_global_string_ptr(&fmt, "fmt").unwrap();
                let mut call_args: Vec<BasicMetadataValueEnum> = vec![fmt_global.as_pointer_value().into()];
                call_args.extend(args);

                self.compile_write_to_file(dest, &call_args)?;

                for ptr in to_free {
                    self.builder.build_call(self.libc_free, &[ptr.into()], "bignum_fmt_free_call").unwrap();
                }
            }
            // `read*(source)*;` -- reads source's whole content (the exact
            // same `read_file_fn` call `input:str ... [from*(source)*]`
            // already uses) and prints it directly via a fixed "%s\n"
            // format string -- the content is always passed as printf's
            // *argument*, never spliced into the format string itself, so
            // a '%' actually present in the file's own content can't be
            // misread as a format specifier. Frees the freshly read buffer
            // right after printf consumes it -- nothing adopts it, unlike
            // `input:str`'s version, which hands it to a named variable.
            Stmt::Read(source) => {
                let path = self.compile_expr(source)?.into_pointer_value();
                let content = self
                    .builder
                    .build_call(self.read_file_fn, &[path.into()], "read_call")
                    .unwrap()
                    .try_as_basic_value()
                    .basic()
                    .unwrap()
                    .into_pointer_value();
                let fmt_global = self.builder.build_global_string_ptr("%s\n", "read_fmt").unwrap();
                self.builder
                    .build_call(self.printf_fn, &[fmt_global.as_pointer_value().into(), content.into()], "read_printf_call")
                    .unwrap();
                self.builder.build_call(self.libc_free, &[content.into()], "read_content_free_call").unwrap();
            }
            Stmt::ExprStmt(expr) => {
                // Not compile_expr(expr): a call to a void function (the
                // common case here -- e.g. a function that just prints) has
                // no return value to unwrap, and compile_expr's Expr::Call
                // arm assumes every call is used in value position and
                // panics otherwise. A bare statement never needs the value.
                if let Expr::Call(name, args) = expr {
                    self.compile_call(name, args)?;
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

                // Hoist any bare literal combined with a bignum anywhere in
                // this loop (condition or body) so it's constructed once,
                // right here, rather than on every iteration -- see
                // `find_hoistable_bignum_literals`/`compile_hoisted_or_literal`.
                // Scoped exactly like a block's own local variables (a
                // dedicated `scopes` frame, pushed here and popped -- with
                // frees -- once the loop is truly done), so an early
                // `return` from inside the loop still frees it correctly
                // via `Stmt::Return`'s existing "free every open scope"
                // pass. Uses `entry_alloca` rather than a plain
                // `build_alloca` here (this setup code can itself run
                // repeatedly if this `while` is nested inside an outer
                // loop) to avoid the same unbounded-dynamic-stack-growth
                // bug `entry_alloca` was already introduced to fix
                // elsewhere.
                let mut literal_sites = Vec::new();
                self.scan_expr_for_bignum_literals(cond, &mut literal_sites);
                self.find_hoistable_bignum_literals(body, &mut literal_sites);
                self.scopes.push(Vec::new());
                let mut frame = HashMap::new();
                for (bits, precision) in literal_sites {
                    if frame.contains_key(&(bits, precision)) {
                        continue;
                    }
                    let n = f64::from_bits(bits);
                    let value = self.coerce_to_bignum(self.context.f64_type().const_float(n).into(), precision);
                    let key = (format!("__hoisted_bignum_lit_{}", self.next_hoisted_lit_id), Type::BigNum(precision));
                    self.next_hoisted_lit_id += 1;
                    let llvm_ty = self.basic_type(key.1);
                    let alloca = self.entry_alloca(llvm_ty, "hoisted_bignum_lit");
                    self.builder.build_store(alloca, value).unwrap();
                    self.declare_scoped(key.clone());
                    self.variables.insert(key.clone(), (alloca, llvm_ty));
                    frame.insert((bits, precision), key);
                }
                self.hoisted_bignum_literals.push(frame);

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
                self.hoisted_bignum_literals.pop();
                self.pop_scope(true);
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
            let temps: Vec<(PointerValue<'ctx>, BasicValueEnum<'ctx>, u32)> = self.bignum_temps.drain(..).collect();
            for (ptr, _, _) in temps {
                self.free_bignum_ptr(ptr);
            }
            let bigint_temps: Vec<(PointerValue<'ctx>, BasicValueEnum<'ctx>)> = self.bigint_temps.drain(..).collect();
            for (ptr, _) in bigint_temps {
                self.free_bigint_ptr(ptr);
            }
            let str_temps: Vec<PointerValue<'ctx>> = self.str_temps.drain(..).collect();
            for ptr in str_temps {
                self.builder.build_call(self.libc_free, &[ptr.into()], "str_temp_free_call").unwrap();
            }
        } else {
            self.bignum_temps.clear();
            self.bigint_temps.clear();
            self.str_temps.clear();
        }
        Ok(())
    }

    fn compile_expr(&mut self, expr: &Expr) -> Result<BasicValueEnum<'ctx>, String> {
        Ok(match expr {
            Expr::Num(n, _) => self.context.f64_type().const_float(*n).into(),
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
                // `bigint` reuses the same `StructValue` shape
                // bignum/array already use, so this can't be told apart
                // from the generic shape-based dispatch below the way
                // int's `IntValue` shape already tells it apart from
                // bool/float -- checked from the AST first, before
                // `inner` is even compiled (compiling it the normal way
                // first, only to discover afterward it needed different
                // handling, would already be too late/wrong).
                if self.expr_is_bigint(inner) {
                    let ptr = self.compile_bigint_expr(expr)?;
                    return Ok(self.wrap_bigint_ptr(ptr));
                }
                let value = self.compile_expr(inner)?;
                match (op, value) {
                    (UnOp::Neg, BasicValueEnum::FloatValue(f)) => {
                        self.builder.build_float_neg(f, "neg").unwrap().into()
                    }
                    // Both `bool` (i1) and `int` (i64) are `IntValue` in
                    // inkwell regardless of width -- bit width tells them
                    // apart, same technique used throughout this file for
                    // the same reason (value_fmt, the binary-op dispatch
                    // below).
                    (UnOp::Not, BasicValueEnum::IntValue(i)) if i.get_type().get_bit_width() == 1 => {
                        self.builder.build_not(i, "not").unwrap().into()
                    }
                    (UnOp::Neg, BasicValueEnum::IntValue(i)) if i.get_type().get_bit_width() != 1 => {
                        self.compile_int_neg(i).into()
                    }
                    (UnOp::Factorial, BasicValueEnum::IntValue(i)) if i.get_type().get_bit_width() != 1 => {
                        self.compile_int_factorial(i).into()
                    }
                    (UnOp::Factorial, BasicValueEnum::FloatValue(f)) => {
                        // Same simplification pow/tetration already make:
                        // always computed at 64-bit regardless of the
                        // operand's declared precision.
                        let f64v = self.coerce_float(f, 64);
                        self.compile_factorial(f64v).into()
                    }
                    (UnOp::Neg, v @ BasicValueEnum::StructValue(_)) => {
                        let src = self.unwrap_bignum_ptr(v);
                        // Preserves the operand's own precision (mirroring
                        // typecheck.rs's identical decision) rather than
                        // forcing the default -- negation doesn't change
                        // the value's magnitude category, so there's no
                        // reason to widen or narrow it.
                        let precision = self.bignum_precision_of_expr(inner).unwrap_or(DEFAULT_BIGNUM_PRECISION);
                        let dst = self.bignum_new(precision);
                        self.builder.build_call(self.bignum.neg, &[dst.into(), src.into()], "bignum_neg_call").unwrap();
                        // Same lifetime story as every other fresh bignum
                        // temporary (see bignum_temps' field docs): nothing
                        // adopts this handle, so the enclosing statement
                        // frees it once it's done being consumed.
                        let wrapped = self.wrap_bignum_ptr(dst);
                        self.bignum_temps.push((dst, wrapped, precision));
                        wrapped
                    }
                    (UnOp::Factorial, v @ BasicValueEnum::StructValue(_)) => {
                        let src = self.unwrap_bignum_ptr(v);
                        let dst = self.compile_bignum_factorial(src);
                        let wrapped = self.wrap_bignum_ptr(dst);
                        self.bignum_temps.push((dst, wrapped, DEFAULT_BIGNUM_PRECISION));
                        wrapped
                    }
                    (op, other) => panic!("unary {op:?} not supported on {other:?}"),
                }
            }
            Expr::Binary(lhs, op, rhs) => {
                // Same reasoning as the `Expr::Unary` fork above: `bigint`
                // shares bignum/array's `StructValue` shape, so this has
                // to be caught from the AST *before* either operand (or
                // the literal-pairing pre-check right below, which
                // assumes a bare literal paired with a non-`IntValue`
                // operand should default to `f64`) ever runs.
                if *op != BinOp::Concat && (self.expr_is_bigint(lhs) || self.expr_is_bigint(rhs)) {
                    let l = self.compile_bigint_expr(lhs)?;
                    let r = self.compile_bigint_expr(rhs)?;
                    return Ok(self.compile_bigint_binary(*op, l, r));
                }
                // A bare literal paired with an `int` operand compiles as
                // an i64 constant directly, not the f64 the generic
                // `Expr::Num` arm would otherwise produce -- mirrors
                // typecheck.rs's identical structural pre-check exactly
                // (the type checker already guarantees the literal is a
                // whole number whenever this resolves to `int`). Both
                // sides being bare literals is unaffected -- still
                // compiles as float/float, same as before.
                let (l, r) = match (lhs.as_ref(), rhs.as_ref()) {
                    (Expr::Num(n, text), other) if !matches!(other, Expr::Num(_, _)) => {
                        let r = self.compile_expr(rhs)?;
                        let l = self.compile_hoisted_or_literal(text, *n, other, &r);
                        (l, r)
                    }
                    (other, Expr::Num(n, text)) if !matches!(other, Expr::Num(_, _)) => {
                        let l = self.compile_expr(lhs)?;
                        let r = self.compile_hoisted_or_literal(text, *n, other, &l);
                        (l, r)
                    }
                    _ => (self.compile_expr(lhs)?, self.compile_expr(rhs)?),
                };
                // `stch` accepts any shape on either side (auto-converting
                // to display text the same way print does) and never
                // widens/promotes its operands -- handle it before any of
                // the numeric-specific matching below, which doesn't apply.
                if *op == BinOp::Concat {
                    let ptr = self.compile_concat(l, r);
                    self.str_temps.push(ptr);
                    return Ok(ptr.into());
                }
                // Two nums of different precisions can't be combined by an
                // LLVM op directly -- widen the narrower one to match first.
                let (l, r) = self.match_float_widths(l, r);
                // Mixing a bignum with a plain num/literal: promote the
                // float side to a fresh bignum -- at the *bignum* side's
                // own precision (via bignum_precision_of_expr), not a
                // fixed default, so combining with e.g. a
                // [precision:1000] bignum doesn't needlessly widen a
                // smaller one up to the default, or silently narrow a
                // larger one down -- so every bignum arm below only ever
                // has to handle bignum-vs-bignum. The promoted value is a
                // genuine new temporary nothing else references, so it's
                // registered for the enclosing statement to free like any
                // other.
                let (l, r) = match (l, r) {
                    (BasicValueEnum::StructValue(_), BasicValueEnum::FloatValue(f)) => {
                        let precision = self.bignum_precision_of_expr(lhs).unwrap_or(DEFAULT_BIGNUM_PRECISION);
                        let coerced = self.coerce_to_bignum(f.into(), precision);
                        let ptr = self.unwrap_bignum_ptr(coerced);
                        self.bignum_temps.push((ptr, coerced, precision));
                        (l, coerced)
                    }
                    (BasicValueEnum::FloatValue(f), BasicValueEnum::StructValue(_)) => {
                        let precision = self.bignum_precision_of_expr(rhs).unwrap_or(DEFAULT_BIGNUM_PRECISION);
                        let coerced = self.coerce_to_bignum(f.into(), precision);
                        let ptr = self.unwrap_bignum_ptr(coerced);
                        self.bignum_temps.push((ptr, coerced, precision));
                        (coerced, r)
                    }
                    other => other,
                };

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
                        BinOp::Concat => unreachable!("Concat is handled earlier, before this match"),
                    },
                    // int (i64) arithmetic -- distinct from the i1
                    // bool-logic arm right below (both are `IntValue` in
                    // inkwell regardless of width). Every arithmetic op
                    // is overflow-checked, crashing with a clear message
                    // rather than silently wrapping (two's-complement) --
                    // the same "loud failure over silent wrong data"
                    // precedent as an out-of-range array index or invalid
                    // `input:num` text.
                    (BasicValueEnum::IntValue(li), BasicValueEnum::IntValue(ri))
                        if li.get_type().get_bit_width() != 1 =>
                    {
                        self.compile_int_binary(*op, li, ri)
                    }
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
                    // Comparisons return a bool (IntValue), not another
                    // bignum -- a structurally different shape than every
                    // other bignum binary op, so this needs its own arm
                    // rather than fitting into the shim_fn dispatch below.
                    // mpf_cmp itself doesn't allocate anything, so there's
                    // no handle to register in bignum_temps here.
                    (BasicValueEnum::StructValue(_), BasicValueEnum::StructValue(_))
                        if matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge) =>
                    {
                        let lp = self.unwrap_bignum_ptr(l);
                        let rp = self.unwrap_bignum_ptr(r);
                        let cmp = self
                            .builder
                            .build_call(self.bignum.cmp, &[lp.into(), rp.into()], "bignum_cmp_call")
                            .unwrap()
                            .try_as_basic_value()
                            .basic()
                            .unwrap()
                            .into_int_value();
                        let zero = self.context.i32_type().const_int(0, true);
                        let predicate = match op {
                            BinOp::Eq => IntPredicate::EQ,
                            BinOp::Ne => IntPredicate::NE,
                            BinOp::Lt => IntPredicate::SLT,
                            BinOp::Gt => IntPredicate::SGT,
                            BinOp::Le => IntPredicate::SLE,
                            BinOp::Ge => IntPredicate::SGE,
                            _ => unreachable!(),
                        };
                        self.builder.build_int_compare(predicate, cmp, zero, "bignum_cmp").unwrap().into()
                    }
                    (BasicValueEnum::StructValue(_), BasicValueEnum::StructValue(_)) if op == &BinOp::Tetration => {
                        let lp = self.unwrap_bignum_ptr(l);
                        let rp = self.unwrap_bignum_ptr(r);
                        // Widens to the larger of the two operands' own
                        // precisions (mirroring typecheck.rs's identical
                        // `check_binary` computation), rather than always
                        // defaulting -- see bignum_precision_of_expr.
                        let result_precision = self
                            .bignum_precision_of_expr(lhs)
                            .unwrap_or(DEFAULT_BIGNUM_PRECISION)
                            .max(self.bignum_precision_of_expr(rhs).unwrap_or(DEFAULT_BIGNUM_PRECISION));
                        let dst = self.compile_bignum_tetration(lp, rp, result_precision);
                        // Same reasoning as the shim_fn arm below: nothing
                        // adopts this handle, so it's registered for the
                        // enclosing statement to free once it's done with it.
                        let wrapped = self.wrap_bignum_ptr(dst);
                        self.bignum_temps.push((dst, wrapped, result_precision));
                        wrapped
                    }
                    (BasicValueEnum::StructValue(_), BasicValueEnum::StructValue(_)) => {
                        let shim_fn = match op {
                            BinOp::Add => self.bignum.add,
                            BinOp::Sub => self.bignum.sub,
                            BinOp::Mul => self.bignum.mul,
                            BinOp::Div => self.bignum.div,
                            BinOp::Pow => self.bignum.pow,
                            _ => panic!("{op:?} not supported on bignum yet"),
                        };
                        let lp = self.unwrap_bignum_ptr(l);
                        let rp = self.unwrap_bignum_ptr(r);
                        // Widens to the larger of the two operands' own
                        // precisions (mirroring typecheck.rs's identical
                        // `check_binary` computation), rather than always
                        // defaulting -- see bignum_precision_of_expr.
                        let result_precision = self
                            .bignum_precision_of_expr(lhs)
                            .unwrap_or(DEFAULT_BIGNUM_PRECISION)
                            .max(self.bignum_precision_of_expr(rhs).unwrap_or(DEFAULT_BIGNUM_PRECISION));

                        // A chain of the same left-associative op (`a + b +
                        // c + d`, `a x b x c x d`, ..., compiled bottom-up as
                        // nested Binary nodes with the running result always
                        // on the *left*) used to allocate a fresh handle for
                        // *every* intermediate step, even though each one is
                        // immediately superseded by the very next step and
                        // never read again. GMP documents that
                        // mpf_add/mpf_sub/mpf_mul/mpf_div's destination may
                        // alias a source operand, so whenever `l` is itself a
                        // not-yet-consumed bignum_temps entry (nothing else
                        // references it -- the same check compile_and_coerce
                        // already uses to adopt a temp directly instead of
                        // copying) at exactly this op's own result
                        // precision, accumulate straight into it instead of
                        // allocating a new destination. Profiled before
                        // implementing, standalone against the real GMP
                        // calls, then confirmed matching in the real
                        // compiler: Add/Sub ~1.8x (one extra term) to ~2.8x
                        // (a 4-op chain); Mul similar (~2.2-2.6x on a 4-op
                        // chain, since multiply is cheap enough that
                        // allocation overhead dominates the same way
                        // addition's does); Div ~1.45x (division is
                        // expensive enough in its own right that the
                        // allocation this removes is a smaller slice of the
                        // total, but still a real, verified win).
                        //
                        // `Pow` doesn't fit the left-only check above: `xx`
                        // parses *right*-associative (see parse_power in
                        // parser.rs), so a real `a xx b xx c` chain builds as
                        // Binary(a, Pow, Binary(b, Pow, c)) -- the reusable
                        // intermediate is the *exponent* (the right
                        // operand), not the base. bignum_pow's own shim
                        // fully reads and converts its exponent argument to
                        // a plain integer *before* ever touching the
                        // destination, so `bignum_pow(dst, base, exp)` with
                        // `dst` aliasing `exp` is safe -- verified
                        // independently (this is dst-aliases-*second*-
                        // argument, a different case than the dst-aliases-
                        // first-argument aliasing already relied on for
                        // Add/Sub/Mul/Div above). Only checked when the left
                        // check didn't already match, since a given op can
                        // only reuse one side's handle as its destination.
                        let reused_left = matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div)
                            .then(|| self.bignum_temps.iter().position(|(_, v, p)| *v == l && *p == result_precision))
                            .flatten();
                        let reused_right = (reused_left.is_none() && *op == BinOp::Pow)
                            .then(|| self.bignum_temps.iter().position(|(_, v, p)| *v == r && *p == result_precision))
                            .flatten();
                        let dst = if let Some(idx) = reused_left {
                            self.bignum_temps.remove(idx);
                            lp
                        } else if let Some(idx) = reused_right {
                            self.bignum_temps.remove(idx);
                            rp
                        } else {
                            self.bignum_new(result_precision)
                        };
                        self.builder.build_call(shim_fn, &[dst.into(), lp.into(), rp.into()], "bignum_op_call").unwrap();
                        // Nothing else ever adopts this handle -- whatever
                        // consumes it (a store via coerce_to_bignum, a print,
                        // an enclosing binary op) only reads or copies from
                        // it. Registered here so the end of whichever
                        // statement this expression is part of can free it.
                        let wrapped = self.wrap_bignum_ptr(dst);
                        self.bignum_temps.push((dst, wrapped, result_precision));
                        wrapped
                    }
                    (l, r) => panic!("binary {op:?} used with mismatched operand types {l:?} / {r:?}"),
                }
            }
            Expr::Call(name, args) => self
                .compile_call(name, args)?
                .expect("function used in expression position must return a value"),
            Expr::ArrayLiteral(_) => {
                panic!("array literal must be compiled via compile_and_coerce with a known target type")
            }
            Expr::ArrayIndex(name, ty, index) => {
                let elem = match ty {
                    Type::Array(elem) => *elem,
                    other => panic!("ArrayIndex on non-array type {other:?} -- should have been caught by the type checker"),
                };
                let (ptr, llvm_ty) = *self
                    .variables
                    .get(&(name.clone(), *ty))
                    .ok_or_else(|| format!("undefined variable '{name}' of type {ty:?}"))?;
                let array_value = self.builder.build_load(llvm_ty, ptr, name).unwrap();
                let handle = self.unwrap_bignum_ptr(array_value);
                let index_value = self.compile_expr(index)?.into_float_value();
                let index_i64 = self
                    .builder
                    .build_float_to_signed_int(index_value, self.context.i64_type(), "array_index_i64")
                    .unwrap();
                let slot_ptr = self
                    .builder
                    .build_call(self.array_get_ptr_fn, &[handle.into(), index_i64.into()], "array_index_get_ptr")
                    .unwrap()
                    .try_as_basic_value()
                    .basic()
                    .unwrap()
                    .into_pointer_value();
                let elem_llvm_ty = self.basic_type(elem.as_type());
                self.builder.build_load(elem_llvm_ty, slot_ptr, "array_index_load").unwrap()
            }
            Expr::Length(array_expr) => {
                let array_value = self.compile_expr(array_expr)?;
                let handle = self.unwrap_bignum_ptr(array_value);
                let length_i64 = self
                    .builder
                    .build_call(self.array_length_fn, &[handle.into()], "length_call")
                    .unwrap()
                    .try_as_basic_value()
                    .basic()
                    .unwrap()
                    .into_int_value();
                self.builder
                    .build_signed_int_to_float(length_i64, self.context.f64_type(), "length_as_num")
                    .unwrap()
                    .into()
            }
        })
    }

    /// Compiles a call to `name`, coercing each argument to the callee's
    /// declared parameter type first (the same way storing a value into a
    /// variable coerces it to that variable's declared type) -- without
    /// this, passing e.g. a bare literal to a `[precision:16]` parameter
    /// would be an LLVM type mismatch. If the callee returns a bignum, the
    /// result is registered in `bignum_temps` like any other freshly
    /// produced bignum value: the callee always hands off a value nothing
    /// else has a claim to, whether the result is actually used (an
    /// `Expr::Call` in value position) or discarded (a bare call
    /// statement) -- either way it must eventually be freed.
    fn compile_call(&mut self, name: &str, args: &[Expr]) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let function = *self.functions.get(name).ok_or_else(|| format!("undefined function '{name}'"))?;
        let (param_types, return_type) = self
            .function_sigs
            .get(name)
            .ok_or_else(|| format!("undefined function '{name}'"))?
            .clone();
        let arg_values: Vec<BasicMetadataValueEnum> = args
            .iter()
            .zip(param_types.iter())
            .map(|(a, &pty)| Ok(self.compile_and_coerce(a, pty)?.into()))
            .collect::<Result<_, String>>()?;
        let call = self.builder.build_call(function, &arg_values, "call").unwrap();
        let result = call.try_as_basic_value().basic();
        if let (Type::BigNum(precision), Some(result)) = (return_type, result) {
            let ptr = self.unwrap_bignum_ptr(result);
            self.bignum_temps.push((ptr, result, precision));
        }
        if let (Type::BigInt, Some(result)) = (return_type, result) {
            let ptr = self.unwrap_bigint_ptr(result);
            self.bigint_temps.push((ptr, result));
        }
        if let (Type::Str, Some(result)) = (return_type, result) {
            self.str_temps.push(result.into_pointer_value());
        }
        Ok(result)
    }

    /// Compiles `segments` into a combined printf-style format string
    /// (with the trailing newline print/overwrite always add) plus its
    /// argument list and any bignum-formatted buffers that need freeing
    /// once whichever function actually consumes them (`printf` or
    /// `fprintf`) is done. Shared by `Stmt::Print` and `Stmt::Overwrite`,
    /// which differ only in *where* the result gets written.
    fn compile_print_segments(
        &mut self,
        segments: &[PrintSegment],
    ) -> Result<(String, Vec<BasicMetadataValueEnum<'ctx>>, Vec<PointerValue<'ctx>>), String> {
        let mut fmt = String::new();
        let mut args: Vec<BasicMetadataValueEnum> = Vec::new();
        let mut to_free: Vec<PointerValue<'ctx>> = Vec::new();
        for seg in segments {
            match seg {
                // Literal text is inserted as-is, except any '%' it
                // contains must be escaped so printf's own format parser
                // doesn't mistake it for a specifier.
                PrintSegment::Str(s) => fmt.push_str(&s.replace('%', "%%")),
                PrintSegment::Expr(e) => {
                    let value = self.compile_expr(e)?;
                    let (frag, arg, maybe_free) = self.value_fmt(value);
                    fmt.push_str(frag);
                    args.push(arg);
                    if let Some(ptr) = maybe_free {
                        to_free.push(ptr);
                    }
                }
            }
        }
        fmt.push('\n');
        Ok((fmt, args, to_free))
    }

    /// Backs `print`'s (optional) and `overwrite`'s (required)
    /// `[to*(dest)*]` clause: opens `dest` (a `str`/`file` path) for
    /// writing -- crashing with a clear message via `cyborg_fopen_or_die`
    /// if it can't be opened, rather than emitting our own null-check IR
    /// -- writes `call_args` (already shaped like a `printf` call, format
    /// string first) via `fprintf`, then closes the file. Always
    /// overwrites the destination's entire content; there's no append yet.
    fn compile_write_to_file(&mut self, dest: &Expr, call_args: &[BasicMetadataValueEnum<'ctx>]) -> Result<(), String> {
        let dest_value = self.compile_expr(dest)?;
        let path_ptr = dest_value.into_pointer_value();
        let mode = self.builder.build_global_string_ptr("w", "write_mode").unwrap();
        let file = self
            .builder
            .build_call(self.fopen_or_die_fn, &[path_ptr.into(), mode.as_pointer_value().into()], "fopen_call")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();

        let mut fprintf_args: Vec<BasicMetadataValueEnum> = vec![file.into()];
        fprintf_args.extend(call_args.iter().copied());
        self.builder.build_call(self.fprintf_fn, &fprintf_args, "fprintf_call").unwrap();
        self.builder.build_call(self.fclose_fn, &[file.into()], "fclose_call").unwrap();
        Ok(())
    }

    /// The printf-style format fragment (no surrounding text), matching
    /// call argument, and -- only for a bignum's formatted string, which is
    /// a fresh malloc'd buffer nothing else references -- a pointer the
    /// caller must free once printf has actually consumed it. Used to
    /// build print's combined format string across all of its segments.
    fn value_fmt(
        &self,
        value: BasicValueEnum<'ctx>,
    ) -> (&'static str, BasicMetadataValueEnum<'ctx>, Option<PointerValue<'ctx>>) {
        match value {
            // printf's varargs ABI expects `double` regardless of num's
            // declared precision (C's default argument promotion, which we
            // have to do explicitly since LLVM won't do it for us).
            BasicValueEnum::FloatValue(f) => ("%g", self.coerce_float(f, 64).into(), None),
            BasicValueEnum::PointerValue(p) => ("%s", p.into(), None),
            // Both `bool` (i1) and `int` (i64) are `IntValue` in inkwell
            // regardless of width -- bit width is what actually tells
            // them apart here, the same technique `float_bit_width`
            // already uses to distinguish num's precisions.
            BasicValueEnum::IntValue(i) if i.get_type().get_bit_width() == 1 => {
                // The actual value is only known at runtime (it could
                // come from a comparison, a variable, anything), so
                // picking "true" vs "false" text needs a runtime select
                // between the two string constants, not a compile-time
                // choice.
                let true_str = self.builder.build_global_string_ptr("true", "true_str").unwrap();
                let false_str = self.builder.build_global_string_ptr("false", "false_str").unwrap();
                let chosen = self
                    .builder
                    .build_select(i, true_str.as_pointer_value(), false_str.as_pointer_value(), "bool_str")
                    .unwrap();
                ("%s", chosen.into(), None)
            }
            // printf's vararg convention needs the full width regardless
            // of int's own declared precision -- sign-extend narrower
            // widths up to a full i64 first (always safe, never the
            // narrowing/overflow-checked direction of coerce_int_width).
            BasicValueEnum::IntValue(i) => ("%lld", self.coerce_int_width(i, 64).into(), None),
            // `bigint` is checked first: it's also a `StructValue` (see
            // `bigint_struct_ty`'s own docs for why), and the two are
            // told apart here by actual LLVM type, not by shape alone --
            // the one place in this file that distinction has to be made
            // from a bare runtime value with no accompanying static type.
            BasicValueEnum::StructValue(_) if self.is_bigint_value(value) => {
                let ptr = self.unwrap_bigint_ptr(value);
                let str_ptr = self
                    .builder
                    .build_call(self.bigint.to_string, &[ptr.into()], "bigint_to_string_call")
                    .unwrap()
                    .try_as_basic_value()
                    .basic()
                    .unwrap();
                let str_ptr = str_ptr.into_pointer_value();
                ("%s", str_ptr.into(), Some(str_ptr))
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
                let str_ptr = str_ptr.into_pointer_value();
                ("%s", str_ptr.into(), Some(str_ptr))
            }
            other => panic!("unsupported value for text formatting: {other:?}"),
        }
    }

    /// `stch`: builds a brand-new, independently-owned `str` from two
    /// values of any shape, reusing `value_fmt`'s exact per-value
    /// formatting (so a non-str operand displays identically to how
    /// `print` would show it). A real two-pass `snprintf` -- first with a
    /// null buffer to measure the exact length, then again into a
    /// freshly `malloc`'d buffer of that size -- rather than a fixed-size
    /// guess, so there's no truncation risk the way the old (removed)
    /// `stch` implementation had.
    fn compile_concat(&mut self, l: BasicValueEnum<'ctx>, r: BasicValueEnum<'ctx>) -> PointerValue<'ctx> {
        let (frag_l, arg_l, free_l) = self.value_fmt(l);
        let (frag_r, arg_r, free_r) = self.value_fmt(r);
        let fmt = format!("{frag_l}{frag_r}");
        let fmt_ptr = self.builder.build_global_string_ptr(&fmt, "stch_fmt").unwrap().as_pointer_value();

        let i8_ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let null_buf = i8_ptr_ty.const_null();
        let zero_size = i64_ty.const_int(0, false);

        let len = self
            .builder
            .build_call(
                self.snprintf_fn,
                &[null_buf.into(), zero_size.into(), fmt_ptr.into(), arg_l, arg_r],
                "stch_size_call",
            )
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_int_value();
        let len64 = self.builder.build_int_z_extend(len, i64_ty, "stch_len64").unwrap();
        let buf_size = self.builder.build_int_add(len64, i64_ty.const_int(1, false), "stch_buf_size").unwrap();
        let buffer = self
            .builder
            .build_call(self.malloc_fn, &[buf_size.into()], "stch_malloc_call")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();

        self.builder
            .build_call(
                self.snprintf_fn,
                &[buffer.into(), buf_size.into(), fmt_ptr.into(), arg_l, arg_r],
                "stch_fill_call",
            )
            .unwrap();

        // Only after both values have actually been formatted into the
        // combined buffer: each operand's own formatted-string buffer (only
        // present for a bignum operand -- see value_fmt) is a separate
        // malloc'd string nothing else references once this call is done
        // reading from it.
        for ptr in [free_l, free_r].into_iter().flatten() {
            self.builder.build_call(self.libc_free, &[ptr.into()], "stch_operand_fmt_free_call").unwrap();
        }

        buffer
    }

    /// int's binary-op entry point -- shared by `compile_expr`'s normal
    /// `Expr::Binary` handling and `compile_int_expr`'s recursive one
    /// (used by `compile_and_coerce`'s propagation of a known `int`
    /// target into a binary expression's own bare-literal operands, see
    /// there for why that's needed). Computes the declared result width
    /// as the larger of the two *actual* operand widths (mirroring
    /// `check_binary`'s identical `lw.max(rw)` in typecheck.rs exactly),
    /// widens both to a full i64 to actually perform the op (arithmetic
    /// always happens at full width -- see `match_int_widths`), then
    /// narrows an arithmetic result back down to that declared width
    /// immediately -- not deferred until/unless the result is later
    /// stored somewhere. This is what makes e.g. two `int[precision:8]`
    /// values added and printed directly (never stored in a variable)
    /// still crash on overflow, rather than silently computing at a wider
    /// width just because nothing narrower ever consumed it. A comparison
    /// result (`bool`, i1) is returned as-is -- there's no width to narrow.
    fn compile_int_binary(&self, op: BinOp, li: IntValue<'ctx>, ri: IntValue<'ctx>) -> BasicValueEnum<'ctx> {
        let result_width = li.get_type().get_bit_width().max(ri.get_type().get_bit_width());
        let (li, ri) = self.match_int_widths(li, ri);
        match self.compile_binary_int_op(op, li, ri) {
            BasicValueEnum::IntValue(iv) if iv.get_type().get_bit_width() != 1 => {
                self.coerce_int_width(iv, result_width).into()
            }
            other => other,
        }
    }

    /// The actual per-operator dispatch, operating on two already-widened
    /// (full i64) operands -- factored out of `compile_int_binary` so
    /// `compile_int_expr`'s recursive case (which needs the *unwidened*
    /// widths for its own narrowing step, computed one level up) can
    /// still reach it directly. Every arithmetic op is overflow-checked,
    /// crashing with a clear message rather than silently wrapping
    /// (two's-complement) -- the same "loud failure over silent wrong
    /// data" precedent as an out-of-range array index or invalid
    /// `input:num` text.
    fn compile_binary_int_op(&self, op: BinOp, li: IntValue<'ctx>, ri: IntValue<'ctx>) -> BasicValueEnum<'ctx> {
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul => self.checked_int_arith(op, li, ri).into(),
            BinOp::Div => self.compile_int_div(li, ri).into(),
            BinOp::Pow => self.compile_int_pow(li, ri).into(),
            BinOp::Tetration => self.compile_int_tetration(li, ri).into(),
            BinOp::Eq => self.builder.build_int_compare(IntPredicate::EQ, li, ri, "eq").unwrap().into(),
            BinOp::Ne => self.builder.build_int_compare(IntPredicate::NE, li, ri, "ne").unwrap().into(),
            BinOp::Lt => self.builder.build_int_compare(IntPredicate::SLT, li, ri, "lt").unwrap().into(),
            BinOp::Gt => self.builder.build_int_compare(IntPredicate::SGT, li, ri, "gt").unwrap().into(),
            BinOp::Le => self.builder.build_int_compare(IntPredicate::SLE, li, ri, "le").unwrap().into(),
            BinOp::Ge => self.builder.build_int_compare(IntPredicate::SGE, li, ri, "ge").unwrap().into(),
            BinOp::And | BinOp::Or => panic!("{op:?} requires bool operands, not int"),
            BinOp::Concat => unreachable!("Concat is handled earlier, before this match"),
        }
    }

    /// Branches to a call to `cyborg_int_die(message)` when `cond` (an
    /// i1) is true, otherwise falls through to a fresh block right after
    /// -- the shared shape every int-specific crash check (overflow,
    /// division by zero, negating `i64::MIN`) uses.
    fn crash_if(&self, cond: IntValue<'ctx>, message: &str) {
        let function = self.current_function();
        let crash_bb = self.context.append_basic_block(function, "int_crash");
        let continue_bb = self.context.append_basic_block(function, "int_continue");
        self.builder.build_conditional_branch(cond, crash_bb, continue_bb).unwrap();

        self.builder.position_at_end(crash_bb);
        let msg_ptr = self.builder.build_global_string_ptr(message, "int_crash_msg").unwrap().as_pointer_value();
        self.builder.build_call(self.int_die_fn, &[msg_ptr.into()], "int_die_call").unwrap();
        self.builder.build_unreachable().unwrap();

        self.builder.position_at_end(continue_bb);
    }

    /// Overflow-checked signed 64-bit `+`/`-`/`x`, crashing with a clear
    /// message if the true result doesn't fit in 64 bits -- rather than
    /// silently wrapping (two's-complement), which is what a plain LLVM
    /// `add`/`sub`/`mul` would do.
    fn checked_int_arith(&self, op: BinOp, li: IntValue<'ctx>, ri: IntValue<'ctx>) -> IntValue<'ctx> {
        let (fn_val, op_name) = match op {
            BinOp::Add => (self.sadd_overflow_fn, "+"),
            BinOp::Sub => (self.ssub_overflow_fn, "-"),
            BinOp::Mul => (self.smul_overflow_fn, "x"),
            _ => panic!("checked_int_arith called with non-arithmetic op {op:?}"),
        };
        let result_struct = self
            .builder
            .build_call(fn_val, &[li.into(), ri.into()], "int_op_call")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_struct_value();
        let result = self.builder.build_extract_value(result_struct, 0, "int_op_result").unwrap().into_int_value();
        let overflowed =
            self.builder.build_extract_value(result_struct, 1, "int_op_overflowed").unwrap().into_int_value();
        self.crash_if(overflowed, &format!("int overflow: result of {op_name} doesn't fit in int"));
        result
    }

    /// Signed `/`, guarding the two ways it can go wrong that a plain
    /// LLVM `sdiv` wouldn't catch on its own: dividing by zero (undefined
    /// behavior at the LLVM level, a hardware trap at runtime), and
    /// `i64::MIN / -1` (the one signed-division case that overflows,
    /// since the true result, `i64::MAX + 1`, doesn't fit).
    fn compile_int_div(&self, li: IntValue<'ctx>, ri: IntValue<'ctx>) -> IntValue<'ctx> {
        let i64_ty = self.context.i64_type();
        let zero = i64_ty.const_int(0, true);
        let is_zero = self.builder.build_int_compare(IntPredicate::EQ, ri, zero, "int_div_zero_check").unwrap();
        self.crash_if(is_zero, "int division by zero");

        let int_min = i64_ty.const_int(i64::MIN as u64, true);
        let neg_one = i64_ty.const_int((-1i64) as u64, true);
        let is_int_min = self.builder.build_int_compare(IntPredicate::EQ, li, int_min, "int_div_min_check").unwrap();
        let is_neg_one = self.builder.build_int_compare(IntPredicate::EQ, ri, neg_one, "int_div_negone_check").unwrap();
        let is_overflow_case = self.builder.build_and(is_int_min, is_neg_one, "int_div_overflow_check").unwrap();
        self.crash_if(is_overflow_case, "int overflow: i64::MIN / -1 doesn't fit in int");

        self.builder.build_int_signed_div(li, ri, "int_div").unwrap()
    }

    /// Negation, guarding the one value whose negation overflows:
    /// `i64::MIN` (its true negation, `i64::MAX + 1`, doesn't fit).
    fn compile_int_neg(&self, i: IntValue<'ctx>) -> IntValue<'ctx> {
        // Negation preserves the operand's own width (see typecheck.rs),
        // so the actual computation widens to i64 (arithmetic always
        // happens at full width -- see match_int_widths), then narrows
        // the result back down to whatever width `i` started at.
        // coerce_int_width's own overflow check on that narrowing step
        // is what catches e.g. negating a narrow width's own minimum
        // value (int8's -128, whose negation -- 128 -- doesn't fit back
        // into int8); the i64::MIN check below only matters when `i` was
        // already a full i64 to begin with, since narrowing i64 to i64
        // is a no-op that wouldn't otherwise catch it.
        let original_width = i.get_type().get_bit_width();
        let widened = self.coerce_int_width(i, 64);
        let i64_ty = self.context.i64_type();
        let int_min = i64_ty.const_int(i64::MIN as u64, true);
        let is_int_min = self.builder.build_int_compare(IntPredicate::EQ, widened, int_min, "int_neg_min_check").unwrap();
        self.crash_if(is_int_min, "int overflow: negating i64::MIN doesn't fit in int");
        let negated = self.builder.build_int_neg(widened, "int_neg").unwrap();
        self.coerce_int_width(negated, original_width)
    }

    /// `xx` on `int`: real integer exponentiation via repeated
    /// overflow-checked multiplication (a genuine runtime loop, mirroring
    /// `compile_tetration`'s shape below), not a float `pow()`
    /// round-trip -- keeping the whole computation exact, matching why
    /// `int` exists at all. A negative exponent would only ever produce
    /// a fractional result (except for base 1/-1, not worth special-
    /// casing), so it crashes rather than silently truncating or
    /// promoting to float.
    fn compile_int_pow(&self, base: IntValue<'ctx>, exponent: IntValue<'ctx>) -> IntValue<'ctx> {
        let i64_ty = self.context.i64_type();
        let zero = i64_ty.const_int(0, true);
        let is_negative = self.builder.build_int_compare(IntPredicate::SLT, exponent, zero, "int_pow_neg_check").unwrap();
        self.crash_if(is_negative, "int power requires a non-negative exponent");

        let function = self.current_function();
        let result_slot = self.entry_alloca(i64_ty.into(), "int_pow_result");
        self.builder.build_store(result_slot, i64_ty.const_int(1, true)).unwrap();
        let counter_slot = self.entry_alloca(i64_ty.into(), "int_pow_i");
        self.builder.build_store(counter_slot, zero).unwrap();

        let cond_bb = self.context.append_basic_block(function, "int_pow_cond");
        let body_bb = self.context.append_basic_block(function, "int_pow_body");
        let end_bb = self.context.append_basic_block(function, "int_pow_end");
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(cond_bb);
        let counter = self.builder.build_load(i64_ty, counter_slot, "int_pow_i_load").unwrap().into_int_value();
        let keep_going = self.builder.build_int_compare(IntPredicate::SLT, counter, exponent, "int_pow_test").unwrap();
        self.builder.build_conditional_branch(keep_going, body_bb, end_bb).unwrap();

        self.builder.position_at_end(body_bb);
        let current = self.builder.build_load(i64_ty, result_slot, "int_pow_result_load").unwrap().into_int_value();
        let next = self.checked_int_arith(BinOp::Mul, current, base);
        self.builder.build_store(result_slot, next).unwrap();
        let counter_next = self.builder.build_int_add(counter, i64_ty.const_int(1, true), "int_pow_i_next").unwrap();
        self.builder.build_store(counter_slot, counter_next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(end_bb);
        self.builder.build_load(i64_ty, result_slot, "int_pow_final").unwrap().into_int_value()
    }

    /// `xxx` on `int`: same shape as `compile_tetration` below (height
    /// copies of `base`, only known at runtime) but calling
    /// `compile_int_pow` at each step instead of libm's `pow` -- real
    /// integer exponentiation throughout, so overflow is caught exactly
    /// (tetration grows astronomically fast, so this crashes almost
    /// immediately for any base/height beyond the smallest values, which
    /// is the correct behavior, not a bug).
    fn compile_int_tetration(&self, base: IntValue<'ctx>, height: IntValue<'ctx>) -> IntValue<'ctx> {
        let function = self.current_function();
        let i64_ty = self.context.i64_type();

        let result_slot = self.entry_alloca(i64_ty.into(), "int_tet_result");
        self.builder.build_store(result_slot, base).unwrap();
        let counter_slot = self.entry_alloca(i64_ty.into(), "int_tet_i");
        self.builder.build_store(counter_slot, i64_ty.const_int(2, true)).unwrap();

        let cond_bb = self.context.append_basic_block(function, "int_tet_cond");
        let body_bb = self.context.append_basic_block(function, "int_tet_body");
        let end_bb = self.context.append_basic_block(function, "int_tet_end");
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(cond_bb);
        let counter = self.builder.build_load(i64_ty, counter_slot, "int_tet_i_load").unwrap().into_int_value();
        let keep_going = self.builder.build_int_compare(IntPredicate::SLE, counter, height, "int_tet_test").unwrap();
        self.builder.build_conditional_branch(keep_going, body_bb, end_bb).unwrap();

        self.builder.position_at_end(body_bb);
        let current = self.builder.build_load(i64_ty, result_slot, "int_tet_result_load").unwrap().into_int_value();
        let next = self.compile_int_pow(base, current);
        self.builder.build_store(result_slot, next).unwrap();
        let counter_next = self.builder.build_int_add(counter, i64_ty.const_int(1, true), "int_tet_i_next").unwrap();
        self.builder.build_store(counter_slot, counter_next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(end_bb);
        self.builder.build_load(i64_ty, result_slot, "int_tet_final").unwrap().into_int_value()
    }

    /// Postfix `!` on `int`: same loop shape as `compile_factorial`
    /// below, but overflow-checked at each multiplication step --
    /// `21!` already exceeds `i64`'s range, unlike `num`'s float version,
    /// which just loses precision gracefully instead of overflowing.
    /// Always returns a full i64 regardless of `n`'s own width (widened
    /// here if narrower) -- factorial's result is always the default
    /// width, same as `num`'s own factorial forcing a fixed precision
    /// (see typecheck.rs), so there's no "original width" to narrow back
    /// to the way negation has.
    fn compile_int_factorial(&self, n: IntValue<'ctx>) -> IntValue<'ctx> {
        let function = self.current_function();
        let i64_ty = self.context.i64_type();
        let n = self.coerce_int_width(n, 64);

        let result_slot = self.entry_alloca(i64_ty.into(), "int_fact_result");
        self.builder.build_store(result_slot, i64_ty.const_int(1, true)).unwrap();
        let counter_slot = self.entry_alloca(i64_ty.into(), "int_fact_i");
        self.builder.build_store(counter_slot, i64_ty.const_int(2, true)).unwrap();

        let cond_bb = self.context.append_basic_block(function, "int_fact_cond");
        let body_bb = self.context.append_basic_block(function, "int_fact_body");
        let end_bb = self.context.append_basic_block(function, "int_fact_end");
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(cond_bb);
        let counter = self.builder.build_load(i64_ty, counter_slot, "int_fact_i_load").unwrap().into_int_value();
        let keep_going = self.builder.build_int_compare(IntPredicate::SLE, counter, n, "int_fact_test").unwrap();
        self.builder.build_conditional_branch(keep_going, body_bb, end_bb).unwrap();

        self.builder.position_at_end(body_bb);
        let current = self.builder.build_load(i64_ty, result_slot, "int_fact_result_load").unwrap().into_int_value();
        let next = self.checked_int_arith(BinOp::Mul, current, counter);
        self.builder.build_store(result_slot, next).unwrap();
        let counter_next = self.builder.build_int_add(counter, i64_ty.const_int(1, true), "int_fact_i_next").unwrap();
        self.builder.build_store(counter_slot, counter_next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(end_bb);
        self.builder.build_load(i64_ty, result_slot, "int_fact_final").unwrap().into_int_value()
    }

    /// `xxx`: a xxx b = a ^ (a ^ (a ^ ... )) with `b` copies of `a`. `b` is
    /// only known at runtime, so this is an actual loop (mirroring how
    /// `while` is compiled), not a fixed chain of multiplications.
    fn compile_tetration(&mut self, base: FloatValue<'ctx>, height: FloatValue<'ctx>) -> FloatValue<'ctx> {
        let function = self.current_function();
        let i64_ty = self.context.i64_type();
        let f64_ty = self.context.f64_type();

        let height_int = self.builder.build_float_to_signed_int(height, i64_ty, "tet_height").unwrap();

        let result_slot = self.entry_alloca(f64_ty.into(), "tet_result");
        self.builder.build_store(result_slot, base).unwrap();
        let counter_slot = self.entry_alloca(i64_ty.into(), "tet_i");
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

    /// bignum's `xxx`, same shape as `compile_tetration` above (a runtime
    /// loop, since height is only known at runtime) but using bignum_pow
    /// at each step instead of libm's pow. Unlike num's version, each
    /// step's destination is a fresh heap handle, so the *previous* step's
    /// handle has to be explicitly freed before it's overwritten -- left
    /// unfreed, this would leak once per tetration step, the same class
    /// of bug the scope-based cleanup elsewhere fixed for named variables.
    /// Returns a raw (unwrapped) handle, matching how the plain shim_fn
    /// arm hands its `dst` back to the caller to wrap once. `precision`
    /// is the caller's already-computed `max(base_precision,
    /// height_precision)` (see `bignum_precision_of_expr`), used for
    /// every intermediate step -- not a fixed default.
    fn compile_bignum_tetration(
        &mut self,
        base: PointerValue<'ctx>,
        height: PointerValue<'ctx>,
        precision: u32,
    ) -> PointerValue<'ctx> {
        let function = self.current_function();
        let i64_ty = self.context.i64_type();

        let height_int = self
            .builder
            .build_call(self.bignum.get_i64, &[height.into()], "tet_bignum_height")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_int_value();

        // One handle for the whole loop, mutated in place every iteration
        // -- used to be a fresh `bignum_new` per iteration, immediately
        // freeing the previous one, even though bignum_pow's destination
        // may safely alias its own exponent argument (it fully reads and
        // converts the exponent to a plain integer before ever touching
        // the destination, so `bignum_pow(acc, base, acc)` is safe -- the
        // same aliasing GMP already documents for the direct-source case,
        // verified independently here since this is dst-aliases-*second*-
        // argument rather than dst-aliases-first). `acc` itself (the
        // pointer) never changes across iterations -- only the value it
        // points to does -- so it needs no alloca/store/load slot at all,
        // unlike the loop counter: it's a single SSA value defined before
        // the loop and used identically in every block that follows.
        let acc = self.bignum_new(precision);
        self.builder.build_call(self.bignum.copy, &[acc.into(), base.into()], "tet_bignum_init_copy").unwrap();
        let counter_slot = self.entry_alloca(i64_ty.into(), "tet_bignum_i");
        self.builder.build_store(counter_slot, i64_ty.const_int(2, true)).unwrap();

        let cond_bb = self.context.append_basic_block(function, "tet_bignum_cond");
        let body_bb = self.context.append_basic_block(function, "tet_bignum_body");
        let end_bb = self.context.append_basic_block(function, "tet_bignum_end");
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(cond_bb);
        let counter = self.builder.build_load(i64_ty, counter_slot, "tet_bignum_i_load").unwrap().into_int_value();
        let keep_going = self
            .builder
            .build_int_compare(IntPredicate::SLE, counter, height_int, "tet_bignum_test")
            .unwrap();
        self.builder.build_conditional_branch(keep_going, body_bb, end_bb).unwrap();

        self.builder.position_at_end(body_bb);
        self.builder.build_call(self.bignum.pow, &[acc.into(), base.into(), acc.into()], "tet_bignum_pow").unwrap();
        let counter_next = self.builder.build_int_add(counter, i64_ty.const_int(1, true), "tet_bignum_i_next").unwrap();
        self.builder.build_store(counter_slot, counter_next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(end_bb);
        acc
    }

    /// Postfix `!` on `num`/`numw`: `n` is only known at runtime, so this is
    /// an actual loop (mirroring `compile_tetration`'s shape), multiplying
    /// 1 by every whole number from 2 up to `n`. Counter stays an i64 for
    /// loop control; converted to float each step to multiply into the
    /// running product.
    fn compile_factorial(&mut self, n: FloatValue<'ctx>) -> FloatValue<'ctx> {
        let function = self.current_function();
        let i64_ty = self.context.i64_type();
        let f64_ty = self.context.f64_type();

        let n_int = self.builder.build_float_to_signed_int(n, i64_ty, "fact_n").unwrap();

        let result_slot = self.entry_alloca(f64_ty.into(), "fact_result");
        self.builder.build_store(result_slot, f64_ty.const_float(1.0)).unwrap();
        let counter_slot = self.entry_alloca(i64_ty.into(), "fact_i");
        self.builder.build_store(counter_slot, i64_ty.const_int(2, true)).unwrap();

        let cond_bb = self.context.append_basic_block(function, "fact_cond");
        let body_bb = self.context.append_basic_block(function, "fact_body");
        let end_bb = self.context.append_basic_block(function, "fact_end");
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(cond_bb);
        let counter = self.builder.build_load(i64_ty, counter_slot, "fact_i_load").unwrap().into_int_value();
        let keep_going = self
            .builder
            .build_int_compare(IntPredicate::SLE, counter, n_int, "fact_test")
            .unwrap();
        self.builder.build_conditional_branch(keep_going, body_bb, end_bb).unwrap();

        self.builder.position_at_end(body_bb);
        let current = self.builder.build_load(f64_ty, result_slot, "fact_result_load").unwrap().into_float_value();
        let counter_f = self.builder.build_signed_int_to_float(counter, f64_ty, "fact_i_f").unwrap();
        let next = self.builder.build_float_mul(current, counter_f, "fact_mul").unwrap();
        self.builder.build_store(result_slot, next).unwrap();
        let counter_next = self.builder.build_int_add(counter, i64_ty.const_int(1, true), "fact_i_next").unwrap();
        self.builder.build_store(counter_slot, counter_next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(end_bb);
        self.builder.build_load(f64_ty, result_slot, "fact_final").unwrap().into_float_value()
    }

    /// bignum's postfix `!`, same shape as `compile_bignum_tetration` --
    /// each step's destination is a fresh heap handle, so the previous
    /// step's handle is explicitly freed before being overwritten, and the
    /// per-step multiplier (the loop counter, always a small whole number)
    /// is built via `bignum_set_d` and freed once consumed. Returns a raw
    /// (unwrapped) handle, matching the tetration/shim_fn convention.
    fn compile_bignum_factorial(&mut self, n: PointerValue<'ctx>) -> PointerValue<'ctx> {
        let function = self.current_function();
        let i64_ty = self.context.i64_type();
        let f64_ty = self.context.f64_type();
        let bignum_ty = self.bignum_struct_type();

        let n_int = self
            .builder
            .build_call(self.bignum.get_i64, &[n.into()], "fact_bignum_n")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_int_value();

        let initial = self.bignum_new(DEFAULT_BIGNUM_PRECISION);
        self.builder.build_call(self.bignum.set_d, &[initial.into(), f64_ty.const_float(1.0).into()], "fact_bignum_init").unwrap();
        let result_slot = self.entry_alloca(bignum_ty.into(), "fact_bignum_result");
        self.builder.build_store(result_slot, self.wrap_bignum_ptr(initial)).unwrap();
        let counter_slot = self.entry_alloca(i64_ty.into(), "fact_bignum_i");
        self.builder.build_store(counter_slot, i64_ty.const_int(2, true)).unwrap();

        let cond_bb = self.context.append_basic_block(function, "fact_bignum_cond");
        let body_bb = self.context.append_basic_block(function, "fact_bignum_body");
        let end_bb = self.context.append_basic_block(function, "fact_bignum_end");
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(cond_bb);
        let counter = self.builder.build_load(i64_ty, counter_slot, "fact_bignum_i_load").unwrap().into_int_value();
        let keep_going = self
            .builder
            .build_int_compare(IntPredicate::SLE, counter, n_int, "fact_bignum_test")
            .unwrap();
        self.builder.build_conditional_branch(keep_going, body_bb, end_bb).unwrap();

        self.builder.position_at_end(body_bb);
        let current_wrapped = self.builder.build_load(bignum_ty, result_slot, "fact_bignum_result_load").unwrap();
        let current_ptr = self.unwrap_bignum_ptr(current_wrapped);
        let counter_f = self.builder.build_signed_int_to_float(counter, f64_ty, "fact_bignum_i_f").unwrap();
        let counter_bignum = self.bignum_new(DEFAULT_BIGNUM_PRECISION);
        self.builder.build_call(self.bignum.set_d, &[counter_bignum.into(), counter_f.into()], "fact_bignum_i_set").unwrap();
        let next = self.bignum_new(DEFAULT_BIGNUM_PRECISION);
        self.builder.build_call(self.bignum.mul, &[next.into(), current_ptr.into(), counter_bignum.into()], "fact_bignum_mul").unwrap();
        self.free_bignum_ptr(current_ptr);
        self.free_bignum_ptr(counter_bignum);
        self.builder.build_store(result_slot, self.wrap_bignum_ptr(next)).unwrap();
        let counter_next = self.builder.build_int_add(counter, i64_ty.const_int(1, true), "fact_bignum_i_next").unwrap();
        self.builder.build_store(counter_slot, counter_next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(end_bb);
        let final_wrapped = self.builder.build_load(bignum_ty, result_slot, "fact_bignum_final").unwrap();
        self.unwrap_bignum_ptr(final_wrapped)
    }

    fn current_function(&self) -> FunctionValue<'ctx> {
        self.builder
            .get_insert_block()
            .unwrap()
            .get_parent()
            .unwrap()
    }

    /// Builds `alloca` in the current function's *entry* block rather than
    /// wherever the builder currently sits. An `alloca` instruction that
    /// lives inside a runtime loop's body block is a genuine dynamic
    /// stack allocation on every iteration -- LLVM only reclaims it when
    /// the function returns, not at the end of that loop body -- so scratch
    /// space needed fresh each iteration (an array-copy/append element
    /// staging slot, for instance) must live in the entry block instead,
    /// allocated exactly once no matter how many times the surrounding
    /// code runs. Restores the builder's position afterward.
    fn entry_alloca(&self, ty: BasicTypeEnum<'ctx>, name: &str) -> PointerValue<'ctx> {
        let current_block = self.builder.get_insert_block().unwrap();
        let entry = current_block.get_parent().unwrap().get_first_basic_block().unwrap();
        match entry.get_first_instruction() {
            Some(first) => self.builder.position_before(&first),
            None => self.builder.position_at_end(entry),
        }
        let alloca = self.builder.build_alloca(ty, name).unwrap();
        self.builder.position_at_end(current_block);
        alloca
    }
}
