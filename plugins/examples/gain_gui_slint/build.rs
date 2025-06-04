use slint_build::{compile, CompileError};

fn main() -> Result<(), CompileError> {
    compile("src/window.slint")
}
