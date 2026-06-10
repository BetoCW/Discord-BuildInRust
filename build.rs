//! Incrusta el icono de la aplicación en el `.exe` (Windows) compilando
//! `icon.rc` con `windres` (del toolchain mingw) y pasando el objeto al linker.
//! Si `windres` no está disponible, se omite con un aviso (no rompe la build).

fn main() {
    #[cfg(windows)]
    {
        let out_dir = std::env::var("OUT_DIR").unwrap_or_else(|_| ".".into());
        let obj = format!("{out_dir}/app_icon.o");

        let status = std::process::Command::new("windres")
            .args(["icon.rc", "-O", "coff", "-o", &obj])
            .status();

        match status {
            Ok(s) if s.success() => {
                println!("cargo:rustc-link-arg={obj}");
            }
            _ => {
                println!(
                    "cargo:warning=No se pudo incrustar el icono (¿windres en PATH?); \
                     el .exe se compilará sin icono propio."
                );
            }
        }

        println!("cargo:rerun-if-changed=icon.rc");
        println!("cargo:rerun-if-changed=icon.ico");
    }
}
