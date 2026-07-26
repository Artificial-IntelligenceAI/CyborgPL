mod ast;
mod codegen;
mod lexer;
mod parser;
mod token;
mod typecheck;

use std::path::{Path, PathBuf};
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

/// A `.cyborgsettings` file only takes effect if the `.cyborgpl` program
/// explicitly names it via a top-level `linkto*("...")*;` directive --
/// there's no automatic discovery by filename convention, so a settings
/// file can never influence a program without that program's own source
/// asking for it by path. Both `linkto`'s path and the settings file's own
/// `allow.link` path must be absolute (a hard error otherwise) -- no
/// relative-to-source-directory resolution, so a link always names one
/// specific file unambiguously regardless of where either file lives.
/// The settings file must consent back: its first line must be
/// `allow.link*("...")*`, naming the exact `.cyborgpl` file linking to
/// it. Both paths are canonicalized and compared, so either
/// side naming the wrong file (or a file that doesn't exist) is a hard
/// error -- "loud failure over silently wrong behavior", same precedent
/// as everywhere else in this compiler, and especially important here
/// since a silently-ignored or silently-mismatched link would look
/// identical to it working correctly.
///
/// After the handshake, the rest of the settings file uses the existing
/// `setting.value` format (e.g. `optimize.false`); blank lines are
/// skipped. An unrecognized setting name or bad value is also a hard
/// error.
fn resolve_optimize_setting(link_to: &Option<String>, source_path: Option<&str>) -> bool {
    let Some(link_to) = link_to else {
        // No linkto*(...)*; directive -- no settings file loads at all,
        // regardless of what happens to sit next to the source file.
        return true;
    };
    let Some(source_path) = source_path else {
        eprintln!("linkto*(...)*; requires a source file on disk (not the built-in default program)");
        std::process::exit(1);
    };

    let source_path = Path::new(source_path);
    if !Path::new(link_to).is_absolute() {
        eprintln!("linkto*(...)*; requires an absolute filepath, found {link_to:?}");
        std::process::exit(1);
    }
    let settings_path = PathBuf::from(link_to);

    let Ok(contents) = std::fs::read_to_string(&settings_path) else {
        eprintln!("linkto*(...)*; names {}, which doesn't exist or can't be read", settings_path.display());
        std::process::exit(1);
    };

    let mut lines = contents.lines().enumerate();
    let Some((_, first_line)) = lines.next() else {
        eprintln!(
            "{}: expected 'allow.link*(\"...\")*' as the first line, found an empty file",
            settings_path.display()
        );
        std::process::exit(1);
    };
    let allowed = parse_allow_link(first_line, &settings_path);
    if !Path::new(&allowed).is_absolute() {
        eprintln!(
            "{}: allow.link*(...)*; requires an absolute filepath, found {allowed:?}",
            settings_path.display()
        );
        std::process::exit(1);
    }
    let allowed_path = PathBuf::from(&allowed);

    let source_canon = std::fs::canonicalize(source_path).unwrap_or_else(|e| {
        eprintln!("{}: {e}", source_path.display());
        std::process::exit(1);
    });
    let allowed_canon = std::fs::canonicalize(&allowed_path).unwrap_or_else(|e| {
        eprintln!(
            "{}: allow.link*(...)*; names {}, which doesn't exist or can't be read: {e}",
            settings_path.display(),
            allowed_path.display()
        );
        std::process::exit(1);
    });
    if source_canon != allowed_canon {
        eprintln!(
            "{}: allow.link*(...)*; names {}, which doesn't match the file linking to it, {}",
            settings_path.display(),
            allowed_path.display(),
            source_path.display()
        );
        std::process::exit(1);
    }

    let mut optimize = true;
    for (i, line) in lines {
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

/// Parses a `.cyborgsettings` file's required first line, `allow.link*("...")*`,
/// returning the quoted path. Mirrors the quoted-string-literal convention
/// `linkto*(...)*;` uses on the `.cyborgpl` side, for consistency.
fn parse_allow_link(line: &str, settings_path: &Path) -> String {
    let fail = |detail: &str| -> ! {
        eprintln!(
            "{}: line 1: expected 'allow.link*(\"filepath\")*' as the first line, {detail}",
            settings_path.display()
        );
        std::process::exit(1);
    };

    let line = line.trim();
    let Some(rest) = line.strip_prefix("allow.link*(") else {
        fail(&format!("found {line:?}"));
    };
    let Some(rest) = rest.strip_suffix(")*") else {
        fail(&format!("found {line:?}"));
    };
    let rest = rest.trim();
    let Some(path) = rest.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        fail(&format!("path must be a quoted string, found {rest:?}"));
    };
    path.to_string()
}

fn main() {
    let path = std::env::args().nth(1);
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

    let optimize = resolve_optimize_setting(&program.link_to, path.as_deref());

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
