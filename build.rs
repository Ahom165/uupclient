//! build.rs — sous Windows : intègre l'icône + métadonnées de version à l'exe.
use std::env;

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // winresource appelle $TARGET-windres (x86_64-w64-mingw32-windres sous
        // cross-build windows-gnu) ; l'icône apparaît dans l'Explorateur.
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/app.ico");
        res.set("FileDescription", "UUP dump Client");
        res.set("ProductName", "UUP dump Client");
        res.set("LegalCopyright", "MIT");
        if let Err(e) = res.compile() {
            // Non bloquant : l'exe fonctionne sans ressource intégrée.
            println!("cargo:warning=icône non intégrée : {e}");
        }
    }
    println!("cargo:rerun-if-changed=assets/app.ico");
}
