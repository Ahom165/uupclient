fn main() {
    // Embarque l'icône Windows uniquement quand on compile pour Windows.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("icon.ico");
        res.set("FileDescription", "UUP dump Client");
        res.set("ProductName", "UUP dump Client");
        res.compile().expect("embarquement des ressources Windows (icône)");
    }
}
