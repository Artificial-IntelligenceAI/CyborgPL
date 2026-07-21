mod ast;
mod codegen;
mod lexer;
mod parser;
mod token;

use std::path::Path;
use std::process::Command;

use inkwell::context::Context;

use codegen::Codegen;
use lexer::Lexer;
use parser::Parser;

// Used when no source file is given on the command line.
const DEFAULT_SOURCE: &str = r#"
    fn add(a: num, b: num) -> num {
        return a + b;
    }

    START
        var:num 'x' = 5;
        var:num 'y' = 2.5;
        if x < y {
            print*add(x, y)*;
        } else {
            print*0*;
        }

        var:str 'greeting' = "hello, cyborgpl";
        print*greeting*;

        var:num 'i' = 0;
        while i < 3 {
            print*i*;
            i = i + 1;
        }
    END
"#;

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

    let context = Context::create();
    let mut codegen = Codegen::new(&context, "cyborgpl");
    codegen.compile_program(&program);

    println!("--- LLVM IR ---");
    println!("{}", codegen.module().print_to_string().to_string());

    let obj_path = Path::new("/tmp/cyborgpl_out.o");
    let bin_path = Path::new("/tmp/cyborgpl_out");

    if let Err(e) = codegen.write_object_file(obj_path) {
        eprintln!("codegen error: {e}");
        std::process::exit(1);
    }

    let status = Command::new("cc")
        .arg(obj_path)
        .arg("-o")
        .arg(bin_path)
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
