# CrabPort

<p align="center">
  <strong>现代化的跨平台 SSH / SFTP 客户端，使用 Rust + GPUI 构建</strong>
</p>

<p align="center">
  <a href="README.md">中文</a> · <a href="README.en.md">English</a>
</p>

<p align="center">
  <a href="https://github.com/chi11321/CrabPort/actions/workflows/dev.yml"><img alt="CI" src="https://github.com/chi11321/CrabPort/actions/workflows/dev.yml/badge.svg?branch=dev"></a>
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue.svg"></a>
  <img alt="Platform" src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey.svg">
  <img alt="Rust" src="https://img.shields.io/badge/rust-1.91%2B-orange.svg">
</p>

---

## 简介

CrabPort 旨在实现一个简单易用的跨平台 SSH / Telnet 客户端，集终端与 SFTP 文件管理于一体。使用 Rust 编写，UI 基于 [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui)（Zed 编辑器的 GPU 渲染框架）。

### 核心特性

- **多标签终端** — SSH / Telnet / 串口 / 本地终端，多会话切换
- **SFTP 文件管理** — 可视化目录浏览与批量上传下载
- **SSH 隧道** — Local / Remote / Dynamic（SOCKS）端口转发
- **代理连接** — SOCKS5 / HTTP(S) 代理，按主机独立配置
- **凭据加密存储** — AES-256-GCM 本地加密
- **命令历史与代码片段** — 自动捕获、搜索、快速执行
- **可配置主题色彩** — 多套预设主题，`config.toml` 驱动
- **跨平台** — macOS / Linux / Windows，x64 与 arm64

## 截图

![主界面](imgs/PixPin_2026-07-01_01-19-29.png)

![终端与文件面板](imgs/PixPin_2026-07-01_01-20-00.png)

## 下载安装

### 从 Release 下载预编译版本

前往 [Releases 页面](https://github.com/chi11321/CrabPort/releases) 下载对应平台的最新版本：

| 平台 | 下载文件 | 说明 |
|------|----------|------|
| macOS (Apple Silicon) | `CrabPort-v*-macos-aarch64.dmg` | 打开 `.dmg` 后将 CrabPort 拖入 `/Applications` |
| macOS (Intel) | `CrabPort-v*-macos-x86_64.dmg` | 打开 `.dmg` 后将 CrabPort 拖入 `/Applications` |
| Linux (x64) | `CrabPort-v*-linux-x86_64.AppImage` | 赋予执行权限后双击运行，内置运行时依赖 |
| Linux (arm64) | `CrabPort-v*-linux-aarch64.AppImage` | 赋予执行权限后双击运行，内置运行时依赖 |
| Windows (x64) | `CrabPort-v*-windows-x86_64.zip` | 解压后双击 `CrabPort.exe` 运行 |
| Windows (arm64) | `CrabPort-v*-windows-aarch64.zip` | 解压后双击 `CrabPort.exe` 运行 |

> macOS 版本以 `.dmg` 磁盘镜像分发，Linux 版本以 `.AppImage` 分发（内置 X11 / Wayland / Vulkan / fontconfig 等运行时库，无需手动安装系统依赖），Windows 版本以 `.zip` 分发（受 cargo-bundle v0.11.0 的 MSI 打包 bug 影响，暂未提供 `.msi` 安装包）。

**macOS 提示**：首次打开可能会提示"无法验证开发者"。右键点击应用 → 选择"打开"即可绕过限制，或在终端执行：
```bash
xattr -cr /Applications/CrabPort.app
```

### 从源码构建

#### 前置要求

- **Rust 1.91+**（推荐使用 [rustup](https://rustup.rs/) 安装）
- 平台原生构建工具链

#### 各平台依赖

**macOS**：Xcode Command Line Tools
```bash
xcode-select --install
```

**Linux**（Debian/Ubuntu）：
```bash
sudo apt-get install -y \
  libx11-dev libx11-xcb-dev libxcb1-dev libxcb-randr0-dev \
  libxcb-keysyms1-dev libxcb-icccm4-dev libxcb-image0-dev \
  libxcb-shape0-dev libxcb-xfixes0-dev libxcb-cursor-dev \
  libxkbcommon-dev libxkbcommon-x11-dev \
  libwayland-dev wayland-protocols \
  libgl1-mesa-dev libegl1-mesa-dev libvulkan-dev \
  libfontconfig1-dev libfreetype6-dev \
  libasound2-dev libpulse-dev libdbus-1-dev \
  libssl-dev pkg-config \
  squashfs-tools   # 打包 .AppImage 所需的 mksquashfs
```

**Windows**：MSVC 工具链（随 Visual Studio Build Tools 安装）

#### 编译运行

```bash
# 克隆仓库
git clone https://github.com/chi11321/CrabPort.git
cd CrabPort

# Debug 模式运行
cargo run

# Release 模式编译
cargo build --release
```

#### 打包为各平台安装包

需先安装 [cargo-bundle](https://github.com/burtonageo/cargo-bundle)：

```bash
cargo install cargo-bundle --locked
```

| 平台 | 命令 | 产物 |
|------|------|------|
| macOS | `cargo bundle --release --format dmg` | `target/release/bundle/dmg/CrabPort_*.dmg` |
| Linux | `cargo bundle --release --format appimage` | `target/release/bundle/appimage/CrabPort_*.AppImage` |
| Windows | `cargo build --release` 后手动压缩 `.exe` | `CrabPort.exe`（`.zip`） |

> Windows 暂不使用 cargo-bundle 打包：其 v0.11.0 的 MSI 打包器存在一个将字符串写入二进制列的 bug，因此 CI 与本地均直接压缩 `.exe` 分发。

## 数据存储位置

应用数据存储在系统标准目录下：

| 平台 | 路径 |
|------|------|
| macOS | `~/Library/Application Support/crabport/` |
| Linux | `~/.local/share/crabport/` |
| Windows | `%APPDATA%\crabport\` |

包含以下文件：
- `crabport.db` — SQLite 数据库（主机、凭据、片段、隧道、代理）
- `.key` — AES-256 加密密钥（随机生成，请勿删除，否则无法解密已存凭据）
- `config.toml` — 应用配置（语言等外观设置，原子写入）

## 技术栈

| 领域 | 技术 |
|------|------|
| 语言 | Rust 2024 Edition |
| UI 框架 | [GPUI](https://github.com/zed-industries/zed) |
| UI 组件库 | [gpui-component](https://github.com/longbridge/gpui-component) |
| 动画 | [gpui-animation](https://github.com/chi11321/gpui-animation) |
| SSH 协议 | [russh](https://github.com/Eugeny/russh) |
| SFTP 协议 | [russh-sftp](https://github.com/AspectUnk/russh-sftp) |
| 终端模拟 | [alacritty_terminal](https://github.com/alacritty/alacritty) |
| 数据库 | [rusqlite](https://github.com/rusqlite/rusqlite) (SQLite) |
| 加密 | [aes-gcm](https://github.com/RustCrypto/AEADs) (AES-256-GCM) |
| 异步运行时 | [tokio](https://tokio.rs) + [smol](https://github.com/smol-rs/smol) |
| 国际化 | [rust-i18n](https://github.com/longbridge/rust-i18n) |

## 路线图

- [x] 设置面板（语言）
- [x] 端口转发 / SSH 隧道管理（Local / Remote / Dynamic）
- [x] 代理连接（SOCKS5 / HTTP CONNECT / HTTPS CONNECT）
- [x] Telnet 连接类型
- [x] 可配置主题色彩
- [ ] 设置面板（字体、快捷键自定义）
- [ ] 终端会话同步（多窗口共享）
- [ ] 串口连接类型
- [ ] 插件系统

## 许可证

[Apache License 2.0](LICENSE)

Copyright © 2026 ch1ll321
