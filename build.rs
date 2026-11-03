fn main() {
    #[cfg(windows)]
    embed_resource::compile("app.rc", embed_resource::NONE);
    winfsp::build::winfsp_link_delayload();
}