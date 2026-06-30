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

CrabPort 旨在实现一个简单易用的跨平台 SSH 客户端，集终端与 SFTP 文件管理于一体。使用 Rust 编写，UI 基于 [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui)（Zed 编辑器的 GPU 渲染框架）。

### 核心特性

- **多标签 SSH 终端** — 基于 `russh` + `alacritty_terminal`，支持多会话、多标签切换、ANSI 全彩渲染
- **SFTP 文件管理** — 可视化目录浏览、文件/目录上传下载、多选批量操作
- **凭据安全存储** — 密钥与密码使用 AES-256-GCM 加密，密钥文件本地随机生成
- **历史命令记录** — 自动捕获终端命令历史，支持搜索、保存为代码片段、一键粘贴/执行
- **代码片段管理** — 全局保存常用命令，支持实时搜索与快速执行
- **主机管理** — 连接信息持久化，支持收藏与按最近登录排序
- **SSH 主机密钥验证** — 首次连接提示确认，后续自动校验
- **跨平台** — 原生支持 macOS / Linux / Windows 的 x64 与 arm64 架构

## 截图

![主界面](imgs/PixPin_2026-07-01_01-19-29.png)

![终端与文件面板](imgs/PixPin_2026-07-01_01-20-00.png)

## 下载安装

### 从 Release 下载预编译版本

前往 [Releases 页面](https://github.com/chi11321/CrabPort/releases) 下载对应平台的最新版本：

| 平台 | 下载文件 | 说明 |
|------|----------|------|
| macOS (Apple Silicon) | `CrabPort-v*-macos-aarch64.zip` | 解压后拖入 `/Applications` |
| macOS (Intel) | `CrabPort-v*-macos-x86_64.zip` | 解压后拖入 `/Applications` |
| Linux (x64) | `CrabPort-v*-linux-x86_64.tar.gz` | 解压后运行，或下载 `.AppImage` 双击运行 |
| Linux (arm64) | `CrabPort-v*-linux-aarch64.tar.gz` | 解压后运行 |
| Windows (x64) | `CrabPort-v*-windows-x86_64.zip` | 解压后双击 `CrabPort.exe`，或下载 `.msi` 安装 |
| Windows (arm64) | `CrabPort-v*-windows-aarch64.zip` | 解压后双击 `CrabPort.exe` |

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
  libssl-dev pkg-config
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

# macOS 打包为 .dmg
cargo install cargo-bundle
cargo bundle --release --format dmg
```

## 项目结构

CrabPort 采用 Cargo workspace 组织，各 crate 职责清晰：

```
CrabPort/
├── src/                    # 二进制入口
│   └── main.rs             # 启动 GPUI Application
├── crabport-core/          # 核心基础设施
│   ├── credential.rs       # 主机与凭据数据模型
│   ├── crypto.rs           # AES-256-GCM 加解密
│   ├── store.rs            # SQLite 持久化层
│   ├── profile.rs          # 用户配置目录
│   └── log.rs              # 日志初始化
├── crabport-ssh/           # SSH 后端
│   ├── backend.rs          # russh 会话管理
│   ├── handler.rs          # 连接回调与主机密钥验证
│   ├── keys.rs             # 私钥解析（OpenSSH/PEM）
│   ├── known_hosts.rs      # known_hosts 持久化
│   ├── monitor.rs          # PTY 数据桥接
│   └── transfer/           # SFTP 传输调度
├── crabport-sftp/          # SFTP 后端
│   ├── api.rs              # SFTP 操作抽象 trait
│   ├── backend.rs          # russh-sftp 实现
│   ├── archive.rs          # 目录打包/解包（tar+gz）
│   └── transfer.rs         # 分块传输
├── crabport-terminal/      # 终端抽象
│   └── terminal.rs         # alacritty_terminal 封装
├── crabport-tunnel/        # 隧道（开发中）
└── crabport-ui/            # GPUI 界面层
    ├── src/
    │   ├── app.rs          # 主窗口与标签页管理
    │   ├── views/
    │   │   ├── terminal/   # 终端视图（渲染、选区、字体、配色）
    │   │   ├── panel/      # 右侧面板（SFTP/历史/片段）
    │   │   ├── hosts.rs    # 主机列表
    │   │   ├── snippets.rs # 片段管理
    │   │   └── tunnels.rs  # 隧道管理
    │   ├── windows/        # 设置、关于等辅助窗口
    │   ├── layouts/        # 布局组件（侧边栏、命令面板、连接表单）
    │   └── components/     # 可复用 UI 组件
    ├── assets/             # 图标等静态资源
    └── i18n/               # 多语言翻译（zh-CN / en）
```

## 数据存储位置

应用数据存储在系统标准目录下：

| 平台 | 路径 |
|------|------|
| macOS | `~/Library/Application Support/crabport/` |
| Linux | `~/.local/share/crabport/` |
| Windows | `%APPDATA%\crabport\` |

包含以下文件：
- `crabport.db` — SQLite 数据库（主机、凭据、片段）
- `.key` — AES-256 加密密钥（随机生成，请勿删除，否则无法解密已存凭据）

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

- [ ] 设置面板（主题、字体、快捷键自定义）
- [ ] 端口转发 / SSH 隧道管理
- [ ] 终端会话同步（多窗口共享）
- [ ] 自定义配色方案
- [ ] 插件系统

## 许可证

[Apache License 2.0](LICENSE)

Copyright © 2026 ch1ll321
