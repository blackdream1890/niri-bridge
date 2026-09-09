# 安装与使用

[English](setup.md) · 简体中文

当前开发验证平台为 Ubuntu 26.04 和 Niri 26.04，两台电脑须安装相同版本的 NiriBridge。

## 构建与安装

需要先单独安装 Niri 26.04，参见[官方项目](https://github.com/niri-wm/niri)。NiriBridge 不安装或重配合成器。

桌面界面使用系统 Python、GTK 3、PyGObject 和 Cairo；托盘通过 GIO 实现 StatusNotifierItem，无须额外的 AppIndicator 绑定。

Ubuntu 运行依赖为 `python3-gi`、`python3-gi-cairo` 和 `gir1.2-gtk-3.0`。后端构建需要 C 编译器与仓库固定的 Rust 工具链。配对文件检查和公开证书规范化需要 `openssl` 命令。

先安装运行依赖：

```sh
sudo apt install python3 python3-gi python3-gi-cairo gir1.2-gtk-3.0 openssl acl pkexec
```

预编译发行包经 `SHA256SUMS` 校验并完整解压后，以桌面用户运行安装程序，不加 sudo：

```sh
python3 scripts/install.py
niri-bridge-ui
```

Git 源码先执行 `cargo build --locked --release`。单独提供的源码发行包附带 `vendor/` 与 `.cargo/config.toml`；预先装好工具链与系统依赖后，可以用 `cargo build --frozen --release` 离线构建。

安装后可从应用启动器打开 **NiriBridge**。后端和启动脚本位于 `~/.local/bin`，界面资源位于 `~/.local/share/niri-bridge/ui`，应用入口和图标位于标准用户目录。仅在不存在用户服务时创建服务文件，保留已有自定义服务、身份、配对、权限和配置。

后端二进制变化时，安装程序会重启当前运行的服务。需要先安装、再一起重启两端时使用：

```sh
python3 scripts/install.py --no-restart
systemctl --user restart niri-bridge.service
```

协议或 ALPN 不一致会拒绝连接，升级时应同时更新两端。

## 界面语言

英文是源语言和回退语言，简体中文已完整翻译。默认跟随桌面的消息语言，也可在首次设置或偏好设置中选择 English、简体中文、跟随系统。

精简安装若缺少中文字体，会显示方框字符；可安装 `fonts-noto-cjk`。普通中文桌面通常已经具备所需字体。

切换语言会重建界面，保留未保存的设置、屏幕编辑和当前页面，不重启共享。语言偏好独立保存在界面配置目录下的 `ui-preferences.json`。

也可指定本次启动语言：

```sh
niri-bridge-ui --language en
niri-bridge-ui --language zh_CN
```

## 首次设置与配对

首次打开时选择小写设备名称、可用的入口显示器和参与共享的实体输入设备。程序创建本机身份，不覆盖已有私钥；已有配置会自动加载。

在“设备与配对”中操作：

1. 导出本机公开配对文件，传给另一台电脑。
2. 在本机导入另一台导出的文件。
3. 对照另一台界面，逐组核对完整 SHA-256 指纹。
4. 确认一致后再信任。配对允许共享输入和同步跨屏入口设置。

保存前会再次检查导入文件与刚才核对的指纹是否一致。私钥文件和本机自己的证书会被拒绝，导出仅包含公开证书。

配置目录 `~/.config/niri-bridge` 内包含 `identity.pem`、`identity.key.pem` 和 `peer.pem`。目录仅限所有者访问，私钥权限为 0600。当前生成证书有效期为一年，过期前需要更新身份并重新配对，尚无自动续期。

## 网络与输入权限

一台选择等待另一台连接，通常监听地址为 `0.0.0.0`；另一台选择主动连接并填写对端可达的局域网地址。默认使用 TCP 42420。

在偏好设置中选择实体键盘、鼠标和触摸板。已配置但暂时断开的设备会保留选择，不会被静默移除。原生手势需要选中触摸板稳定路径并开启“共享原生手势”。

“授权访问输入设备”使用现有管理员安装程序。先检查授权范围，再完成操作系统的管理员认证。权限仅针对选定设备和 uinput，通过 `uaccess` 授予活跃桌面用户；共享程序以普通用户运行，不加入整个 input 用户组。

命令行准备方式：

```sh
python3 scripts/device-access.py plan --config ~/.config/niri-bridge/config.toml --output ~/.config/niri-bridge/device-access.json
sudo python3 scripts/device-access.py install --plan ~/.config/niri-bridge/device-access.json
```

若监听端启用了 UFW，生成计划时加入 `--allow-from OTHER_COMPUTER_LAN_IPV4`，仅允许指定的对端私有 IPv4 访问 TCP 42420。安装程序保留防火墙启用状态，不对整个网段开放。已有安装计划与新范围不同时会拒绝覆盖，应先检查并调整授权安装范围。

托管规则位于 `/etc/udev/rules.d/71-niri-bridge-input.rules`，受保护的恢复记录位于 `/var/lib/niri-bridge/device-access.json`。

## 屏幕连接

两端连接且解锁后，在任意一台打开“屏幕连接”：

- 将另一台电脑的屏幕组拖到本机上、下、左或右侧。
- 选择两端用于跨屏的显示器。
- 如果只连接部分边缘，调整高亮的入口范围。
- 点击“保存并同步两端”。

保存时会短暂恢复本地控制，验证两端配置后写入新入口，并用新设置重新连接。其他界面或编辑器的修改会通过配置版本检测，避免静默覆盖。网络中断导致无法确认时，等待重连并检查实际显示的设置，不要假定两端已全部完成。

每台电脑内部的显示器位置仍由 Niri 管理；这里调整电脑之间的跨屏入口。

## 运行与返回本机

界面的启动与暂停控制 `niri-bridge.service`；自动启动设置决定服务是否随 Niri 图形会话启动。关闭窗口或退出界面会保留后台服务。

托盘可重新打开界面、暂停或启动共享、退出界面。托盘图标需要 Waybar 等宿主支持，没有托盘时仍可使用应用启动器。

**Ctrl+Alt+Shift+Escape** 不依赖网络回复，立即返回来源端。普通 Escape 正常转发。接收端的实体输入会结束共享；任一端锁屏则暂停输入并释放捕获和按键状态。

```sh
systemctl --user stop niri-bridge.service
systemctl --user start niri-bridge.service
```

原生触摸板模式在双方已连接且解锁时接管选定触摸板，将帧发往本机或对端虚拟触摸板。初次接管等待所有手指抬起，跨屏时保留触点状态，停止或断线后释放设备。目标端的 Niri `input { touchpad { ... } }` 控制轻触点击、自然滚动和加速度。

## 诊断与恢复

界面可复制现有的 `doctor` 报告，报告不含输入内容、私钥、配对地址或设备序列号。也可使用：

```sh
niri-bridge doctor --json
niri-bridge check-config --config ~/.config/niri-bridge/config.toml
niri-bridge check-session
```

`verify-input` 和触摸板示例属于显式输入测试，详见[测试说明](testing.zh-CN.md)。协议存在或进程正在运行不能代替实际输入验证。

首次经界面修改的配置会备份为 `config.before-ui.toml`。撤销设备权限安装需要管理员认证：

```sh
systemctl --user disable --now niri-bridge.service
sudo python3 scripts/device-access.py uninstall
```

安装程序保留外部修改的规则；仅对同一次启动中未变化的设备节点恢复旧 ACL。无法安全恢复时按其提示重启。身份文件的保留或删除由所有者明确决定。

## 当前限制

- 支持 Linux 键码 1–255，更高编号尚未支持。
- 已在初始硬件验证基于槽位的原生触摸板及三指、四指手势；组合捏合、跨电脑拖拽和长期休眠恢复仍需更广泛测试。
- 当前拓扑为两台 Niri 电脑，其他合成器、平台及多机拓扑尚未验证。
- 证书续期尚未自动化，当前为采用 GPL-3.0-or-later 的早期测试版。

## 升级与卸载

在两台电脑上下载并校验新版本，各自运行 `python3 scripts/install.py --no-restart`，然后重启两端的 `niri-bridge.service`。安装程序保留配置、身份和自定义服务，并记录自身程序文件的哈希，供卸载时核对。

先预览范围，再执行卸载：

```sh
niri-bridge-uninstall --dry-run
niri-bridge-uninstall
```

卸载保留配置、配对身份和被用户编辑过的程序文件，仅停止和移除安装程序自己管理的用户服务。已有自定义服务需要先停止，其定义会保留。

需要同时撤销受管理的输入权限时，明确请求管理员认证：

```sh
niri-bridge-uninstall --revoke-input-access
```

程序会先运行权限卸载工具，再删除程序文件。管理员认证失败或取消时会保留程序文件，共享服务可能已经停止。若卸载时保留了设备授权，原始发行包中的 `scripts/device-access.py` 仍可用于后续恢复。除非打算重新配对，否则保留身份文件。
