// Windows용 빌드일 때만 실행 파일에 앱 아이콘을 넣는다 (다른 OS에서는 아무 일도 하지 않음).
fn main() {
    println!("cargo:rerun-if-changed=packaging/windows/blurscreen.rc");
    println!("cargo:rerun-if-changed=packaging/windows/app.ico");
    embed_resource::compile("packaging/windows/blurscreen.rc", embed_resource::NONE)
        .manifest_optional()
        .unwrap();
}
