# 构建与测试

[English](testing.md) · 简体中文

## 环境

- 当前验证平台：Ubuntu 26.04、Niri 26.04。
- 固定 Rust 工具链：1.98.1，通过 rustup 与 `rust-toolchain.toml` 选择。
- Wayland 客户端使用 Rust 后端，当前工程不要求安装 Wayland、libinput 或 libudev 的开发头文件。
- `doctor` 的 portal 元数据检查使用 `busctl`，禁用自动启动目标服务和交互授权。

## 常规检查

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --locked --release
```

常规测试覆盖 Niri IPC 消息、逻辑边界映射、配置校验、按键状态释放、接收方实体输入接管和锁屏暂停。常规测试不会向当前桌面注入输入或创建捕获窗口。

管理员安装器的纯逻辑测试和用户服务语法检查：

```sh
python3 -B -m unittest discover -s scripts -p 'test_*.py'
systemd-analyze --user verify service/niri-bridge.service
```

## 只读检查

```sh
cargo run --locked -- doctor
cargo run --locked -- doctor --json
cargo run --locked -- check-layout examples/layout.toml
```

诊断报告不包含主机名、IP、显示器序列号或键盘内容。它明确区分接口宣告、权限和未执行的行为测试。查询失败或无桌面会话时返回结构化的不可用信息。

`examples/layout.toml` 是供校验器使用的示例逻辑布局，不是自动发现配置，也不会被应用到桌面。

## 后台隔离输入测试

需要已安装的 Weston（支持 headless/pixman）和 Niri：

```sh
cargo test --locked --test nested_capture -- --ignored --nocapture
cargo test --locked --test encrypted_bridge -- --ignored --nocapture --test-threads=1
```

测试创建权限为 0700 的临时运行目录，启动无显示输出的 Weston 和独立 Niri。Niri 不使用 `--session`，不导入或修改日常会话环境，禁用测试实例的 Xwayland 启动。

合成键鼠只连接测试自行创建的 Wayland socket，测试覆盖：

- 指针进入边缘后获得指针锁、键盘焦点与快捷键抑制。
- 捕获相对移动、键盘、鼠标按钮和滚动事件。
- Escape 与超时能结束捕获。
- 隔离 Niri 会话锁定后丢失焦点并结束捕获。
- 每次退出后 Niri 的测试 layer 确实被移除。

`encrypted_bridge` 另外创建两套隔离 Niri，经相互认证的 TLS 运行同一个协调器，检查双向按键与按钮、边缘返回、接收方实体接管、锁状态、输出重配置及断线释放。实体键盘 fixture 在没有 Wayland 按键的情况下验证转发和紧急返回，并检查两个来源不会重复发送同一按键。

测试结束清理自己创建的服务和临时目录。隔离测试的键盘注入使用测试专用虚拟键盘，锁状态部分使用受控标记；它不能证明实际 uinput、logind 锁屏信号或真实触摸板的所有动作已经通过。另有测试明确证明 Wayland 虚拟键盘直接注入不能触发所测试的 Niri 绑定。

2026-09-08 的验证记录：42 项常规 Rust 测试、4 项安装器测试、两套隔离输入集成测试，以及 fmt、clippy 和用户服务语法检查通过。两台实际 Ubuntu/Niri 设备均运行同一 release 程序；用户确认指针和键盘双向可用，并确认三指、四指手势转发及其他基本操作正常。原生触摸板引入后的时序问题已改为异步可读通知和原始帧时间戳，新增测试检查批量到达时仍保留硬件帧间隔；用户在时序修正部署后确认触摸板速度和跟手程度恢复正常。

## 原生触摸板接收验证

`examples/verify_touchpad.rs` 是显式的实机开发探测，先读取选定触摸板的无身份能力描述，再在目标电脑短暂创建虚拟触摸板，测试三指工作区切换和四指总览并恢复窗口焦点与总览状态。它不会读取或记录实际手势内容：

```sh
cargo run --locked --release --example verify_touchpad -- profile --config ~/.config/niri-bridge/config.toml --output /tmp/touchpad-profile.json
cargo run --locked --release --example verify_touchpad -- verify --profile /tmp/touchpad-profile.json
```

描述文件应来自实际输入来源电脑；在没有实体触摸板的接收电脑上，可使用同一描述文件。探测应在已授权的测试时机运行，不能和用户正常操作混在一起解释结果。实际接收端已取得三指、四指、焦点恢复和总览恢复全部为 true 的结果。

## 真实设备捕获实验

这是显式的交互实验，需要确认当前使用者方便测试。先运行 `doctor` 取得输出名称，例如：

```sh
cargo run --locked -- capture-test --output eDP-1 --edge top --start 0.35 --end 0.65 --seconds 15
```

1. 屏幕顶部中间出现窄蓝线，计时从实验准备好后开始，等待进入也包含在时限内。
2. 将指针移入蓝线后捕获键鼠，此时指针隐藏。
3. 测试触摸板移动、点击、双指滚动和普通测试键。
4. 按 Escape 立即退出；不按时也会在 15 秒后退出。参数最长允许 30 秒，另有独立看门狗处理进程阻塞。

输出仅包含事件计数、是否观察到捕获状态及退出原因，不保留实际按键内容。该实验没有网络连接，也不向其他应用或电脑注入输入。

计数为零可能代表未进入蓝线或相应动作未被测试，不能当作成功结果。真实触摸板返回、本机快捷键抑制和测试后恢复仍需结合实际操作观察。

## 0.2 桌面界面与双端设置

GTK 界面需要 GTK 3、PyGObject 和 Cairo。下列测试使用模拟状态，打开带测试标识的窗口，不控制实际共享服务：

```sh
python3 -B ui/test_i18n.py
python3 -W ignore::DeprecationWarning:gi.events -B ui/test_ui.py
NIRI_BRIDGE_UI_TEST_LANGUAGE=zh_CN python3 -W ignore::DeprecationWarning:gi.events -B ui/test_ui.py
```

翻译测试覆盖全部英文消息、中文目录、占位符和语言偏好存储。窗口测试调用实际按钮处理函数，检查启动加载无写入副作用、两端屏幕请求构造，以及语言切换保留未保存编辑且不重启后端。警告过滤仅针对系统 PyGObject/Python 的兼容性弃用警告，应用异常与 GTK 警告仍可见。

渲染测试仅把自己的窗口绘制到 Cairo PNG，不截取桌面或修改共享剪贴板。调整布局后检查两种语言的所有页面，并清理临时图片。

双端配置集成测试通过真实本地管理接口，将请求送入双方认证的 TLS 协调器，检查两端配置文件实际保存成功；过期配置版本或不可用的对端显示器必须拒绝，且不修改任一配置。

多连接测试在隔离 Niri 中运行两对出入口，并故意打乱一端的配置顺序，验证双向进入、从另一条连接返回、观察窗口实际收到的指针位置，以及恢复焦点后的按键状态。界面测试覆盖添加、选择、移除、重叠校验和切换语言保留草稿；配置测试覆盖旧 `[edge]` 迁移、注释与连接顺序保留、事务锁释放。这些测试不能代替多屏实机验收。

实机界面与同步检查需要两端 Niri 会话解锁。应分别记录模拟窗口、隔离双端文件测试和两台日常电脑上的实际结果。

两台已安装的日常桌面均通过实际托盘注册、菜单读取、打开窗口、暂停、启动及退出检查。配置和自动启动状态保留，锁屏时正确禁用屏幕保存。两端解锁后，分别从两台电脑发起的入口修改均成功更新双端文件；原配置逐字节恢复，两端重新连接。

## 隔离 Linux 内核与 libinput 交接测试

内核回归在 QEMU 中运行，不连接宿主输入设备、显示、网络、监视器或共享文件系统。
Rust 测试和 C 观察器均要求专用虚拟机启动标记，否则拒绝运行。
需要 `qemu-system-x86`、`busybox-static`、`libinput-dev` 和 `libudev-dev`，
以及可读、内置 uinput 的 Ubuntu 内核镜像。CI 从 Ubuntu 签名软件源提取固定版本
测试内核，不将它安装为宿主启动内核。

```sh
python3 -B scripts/test-kernel-input.py --kernel /path/to/test-vmlinuz
```

测试覆盖初始、捕获和下一次触点槽位及停止时仍按住的全部 250 种组合，
再复现旧版解除独占后继续触摸造成的残留触点。真实 libinput 观察器检查重复的
指针移动、三指与四指滑动，并在开启轻触点击时确认交接不产生额外按钮、移动或
手势开始事件。新版正常交接要求没有输入状态错误；旧版恢复允许一次性重复结束
诊断，随后手势必须恢复正常且不再增加错误。测试不打开日常桌面的输入设备。

Python 生命周期测试模拟服务和 socket，覆盖首次打开不启动共享、退出前停止、
旧版及异常停止恢复、独立进程保护、私有升级备份、登录启动迁移，以及自定义入口
和文件保留。界面测试另行检查开始/停止、托盘退出失败和关窗隐藏到托盘。
