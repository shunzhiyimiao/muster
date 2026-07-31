//! macOS 上必须给链接器 `-ObjC`。
//!
//! 症状:运行时崩 `+[NSString stringForAbslStringView:]: unrecognized selector`。
//! 原因:LiveKit 的 WebRTC 静态库里有 Objective-C **分类(category)**,而
//! 链接器默认只拉取被显式引用的符号——分类不产生符号引用,于是被整个剥掉,
//! 到运行时才发现方法不存在。`-ObjC` 强制加载静态库里所有 ObjC 代码。
//!
//! 这类问题编译期毫无征兆,只有真跑才会暴露。

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-ObjC");
    }
}
