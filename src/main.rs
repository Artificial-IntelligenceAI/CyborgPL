mod ast;
mod codegen;
mod lexer;
mod parser;
mod token;
mod typecheck;

use std::path::Path;
use std::process::Command;

use inkwell::context::Context;

use codegen::Codegen;
use lexer::Lexer;
use parser::Parser;
use typecheck::TypeChecker;

// Used when no source file is given on the command line.
const DEFAULT_SOURCE: &str = r#"
    func 'add'*'a': num, 'b': num* -> num {
        return (ref:var:num 'a') + (ref:var:num 'b');
    }

    START
        var:num 'first' = (5);
        var:num 'second' = (2.5);
        if (ref:var:num 'first') < (ref:var:num 'second') {
            print*ref:func 'add'*(ref:var:num 'first'), (ref:var:num 'second')**;
        } else {
            print*(0)*;
        }

        var:str 'greeting' = ("hello, cyborgpl");
        print*(ref:var:str 'greeting')*;

        var:num 'i' = (0);
        while (ref:var:num 'i') < (3) {
            print*(ref:var:num 'i')*;
            ref:var:num 'i' = (ref:var:num 'i') + (1);
        }
    END
"#;

/// A `name.cyborgpl` file's settings live in a sibling `name.cyborgsettings`
/// file in the same directory -- not a CLI flag, so a program's settings
/// travel with it as its own file rather than however it happens to be
/// invoked. Each line is `setting.value` (e.g. `optimize.false`); blank
/// lines are skipped. Missing file entirely means every default applies
/// (currently just `optimize`, defaulting to `true`). An unrecognized
/// setting name or a value that isn't `true`/`false` is a hard error --
/// the same "loud failure over silently wrong behavior" precedent every
/// other part of this compiler already follows (a crash on a typo, not a
/// silently-ignored setting), which matters especially here since a
/// silently-ignored optimize setting would look identical to it working.
fn read_optimize_setting(source_path: &str) -> bool {
    let settings_path = Path::new(source_path).with_extension("cyborgsettings");
    let Ok(contents) = std::fs::read_to_string(&settings_path) else {
        return true;
    };

    let mut optimize = true;
    for (i, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once('.') else {
            eprintln!(
                "{}: line {}: expected 'setting.value', found {line:?}",
                settings_path.display(),
                i + 1
            );
            std::process::exit(1);
        };
        match name {
            "optimize" => {
                optimize = match value {
                    "true" => true,
                    "false" => false,
                    other => {
                        eprintln!(
                            "{}: line {}: 'optimize' must be true or false, found {other:?}",
                            settings_path.display(),
                            i + 1
                        );
                        std::process::exit(1);
                    }
                };
            }
            other => {
                eprintln!("{}: line {}: unrecognized setting '{other}'", settings_path.display(), i + 1);
                std::process::exit(1);
            }
        }
    }
    optimize
}

fn main() {
    let path = std::env::args().nth(1);
    let optimize = path.as_deref().map_or(true, read_optimize_setting);
    let source = match &path {
        Some(path) => std::fs::read_to_string(path).unwrap_or_else(|e| {
            eprintln!("failed to read {path}: {e}");
            std::process::exit(1);
        }),
        None => DEFAULT_SOURCE.to_string(),
    };

    let tokens = Lexer::new(&source).tokenize().unwrap_or_else(|e| {
        eprintln!("lex error: {e}");
        std::process::exit(1);
    });

    let program = Parser::new(tokens).parse_program().unwrap_or_else(|e| {
        eprintln!("parse error: {e}");
        std::process::exit(1);
    });

    if let Err(errors) = TypeChecker::check_program(&program) {
        for e in &errors {
            eprintln!("type error: {e}");
        }
        std::process::exit(1);
    }

    let context = Context::create();
    let mut codegen = Codegen::new(&context, "cyborgpl");
    if let Err(e) = codegen.compile_program(&program) {
        eprintln!("codegen error: {e}");
        std::process::exit(1);
    }

    println!("--- LLVM IR ---");
    println!("{}", codegen.module().print_to_string().to_string());

    let obj_path = Path::new("/tmp/cyborgpl_out.o");
    let bin_path = Path::new("/tmp/cyborgpl_out");

    if let Err(e) = codegen.write_object_file(obj_path, optimize) {
        eprintln!("codegen error: {e}");
        std::process::exit(1);
    }

    let status = Command::new("cc")
        .arg(obj_path)
        .arg("-o")
        .arg(bin_path)
        .arg("-lm") // pow(), for xx/xxx
        .arg(env!("CYBORGPL_FP128_LIB")) // our own soft-float [precision:128] support
        .arg(env!("CYBORGPL_BIGNUM_LIB")) // our GMP shim, for bignum
        .arg(format!("-L{}", env!("CYBORGPL_GMP_LIB_DIR")))
        .arg("-lgmp")
        .arg(format!("-L{}", env!("CYBORGPL_MIMALLOC_LIB_DIR"))) // backs the GMP allocator hook in bignum_shim.c
        .arg("-lmimalloc")
        .arg(env!("CYBORGPL_IO_LIB")) // our stdin shim, for input:str/input:num
        .arg(env!("CYBORGPL_CLOCK_LIB")) // our clock shim, for clock:num
        .arg(env!("CYBORGPL_ARRAY_LIB")) // our array shim, for var:array:TYPE
        .arg(env!("CYBORGPL_INT_LIB")) // our int shim, for var:int overflow/div-by-zero crashes
        .status()
        .expect("failed to invoke cc");
    if !status.success() {
        eprintln!("linking failed");
        std::process::exit(1);
    }

    println!("--- running compiled binary ---");
    let run_status = Command::new(bin_path).status().expect("failed to run compiled binary");
    println!("--- exit code: {:?} ---", run_status.code());
}
