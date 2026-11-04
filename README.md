# AltSnapGroup

Automatically bring snapped windows on the same virtual desktop to the front together, with Alt-drag window controls.

自动将同一虚拟桌面上的贴靠窗口一起显示，并提供 Alt 拖动窗口功能。

[中文](#中文) | [English](#english)

## 中文

### 功能

- **自动绑定贴靠窗口**：同一虚拟桌面上被 Windows 标记为贴靠的窗口合并为一个绑定组；不足两个窗口时不建立组。即使它们属于不同的 Windows 贴靠组，也会合并。不同虚拟桌面的窗口不会混组。
- **一起显示**：激活绑定组中的窗口时，程序将同组其他未最小化窗口前置。
- **同步移动/最小化**：通过托盘开关启用，默认关闭。开启后，移动或最小化组内窗口时，同组窗口跟随；恢复窗口时尝试恢复同组已最小化窗口。调整大小不联动。
- **Alt 窗口操作**：默认按住 `Alt` 加鼠标左键拖动窗口，或按住 `Alt` 加鼠标右键调整窗口大小。开启同步移动后，Alt 左键拖动也会带动同组窗口；调整大小只影响当前窗口。

程序使用 Windows 的 `IsWindowArranged` 检测单个窗口的贴靠状态，并不是读取 Windows 贴靠组成员。如果旧贴靠布局仍然保留，其窗口也可能加入绑定组。

已知问题：最小化并恢复后，绑定组可能失效；该问题尚在排查。

### 使用

1. 运行 `AltSnapGroup.exe`，程序驻留在系统托盘。
2. 在 Windows 中将至少两个窗口贴靠。默认启用自动检测：启动时检测一次，之后根据窗口事件更新绑定组，无需手动选窗。
3. 右键托盘图标，展开 **绑定组** 查看各桌面的窗口标题。菜单中的“桌面 1”等只是列表编号，不保证与 Windows 桌面顺序一致。
4. 激活组中任一窗口，同组其他未最小化窗口会一起前置。
5. 如需同步移动和最小化，在托盘中启用 **同步移动/最小化**；关闭后两种操作均不联动。

### 自动与手动检测

托盘中的 **自动检测贴靠窗口** 控制检测模式：

- **开启**：启动及相关窗口事件触发检测；“立即检测”置灰，手动检测快捷键不注册。
- **关闭**：通过托盘 **立即检测** 或默认快捷键 `Ctrl+Alt+G` 手动刷新。窗口变化不会自动重建绑定组；窗口关闭等情况下，显示结果可能需要手动刷新。

自动模式不做定时扫描。如果某个窗口变化没有产生可用事件，绑定组可能暂时不是最新状态，可切换到手动模式刷新。

### 托盘与配置

托盘还提供 **找回屏幕外窗口**、语言切换、Alt 配置编辑与重新加载、以管理员身份重新启动、开机自启和退出。找回功能会检查可识别的窗口，并将无法操作的标题栏移回显示器，不限于绑定组。

配置文件为 `%APPDATA%\AlwaysBindWindow\settings.json`。目录沿用旧名称以保留已有配置。手动检测快捷键保存在 `hotkey_bind` 中，Alt 操作配置保存在 `alt` 中；编辑时请保留文件内其他字段。托盘的 Alt 配置编辑和重新加载用于调整 Alt 操作。

### 构建

项目面向 Windows；当前功能主要在 Windows 11 上使用。Windows 10 及其他平台的本次变更尚未完整验证。

```bash
git clone https://github.com/XR-stb/AlwaysBindWindow.git
cd AlwaysBindWindow
cargo build --release
```

生成的程序位于 `target/release/AltSnapGroup.exe`。运行测试：`cargo test`。

## English

### Features

- **Automatic grouping:** All windows that Windows reports as arranged on the same virtual desktop form one group when at least two are present. Separate Windows snap groups on that desktop are combined; windows on different virtual desktops remain separate.
- **Show together:** Activating a group member brings its non-minimized peers forward.
- **Move/minimize together:** Enable this tray option to move or minimize group members together and attempt to restore minimized peers. It is off by default; resizing is not synchronized.
- **Alt window controls:** By default, hold `Alt` and drag with the left mouse button to move a window, or with the right mouse button to resize it. With synchronized movement enabled, Alt-left dragging also moves group members; resizing affects only the current window.

The application detects each window's arranged state using Windows `IsWindowArranged`; it does not read Windows snap-group membership. Windows from an older snap layout may also be included if Windows still reports them as arranged.

Known issue: a group may stop working after windows are minimized and restored; this is still under investigation.

### Getting started

1. Run `AltSnapGroup.exe`; it stays in the system tray.
2. Snap at least two windows in Windows. Automatic detection is enabled by default: the app scans on startup and updates after relevant window events.
3. Right-click the tray icon and expand **Window groups** to see member titles. The displayed desktop numbers are list labels, not guaranteed Windows desktop numbers.
4. Activate a member to bring its non-minimized peers forward.
5. To synchronize movement and minimizing, enable **Move/minimize together** in the tray. Both are disabled when the option is off.

### Detection modes

Use **Automatically detect snapped windows** in the tray menu:

- **On:** Detection runs at startup and after relevant window events. **Detect now** is disabled and its hotkey is not registered.
- **Off:** Use **Detect now** or the default `Ctrl+Alt+G` hotkey to refresh. Window changes do not automatically rebuild groups; a manual refresh may be needed after a window closes.

Automatic mode does not poll periodically. If a change does not generate a usable event, switch to manual mode and refresh.

### Tray and settings

The tray also provides off-screen window recovery, language selection, Alt configuration editing and reload, restart as administrator, auto-start, and exit. Recovery applies to recognizable windows, not just group members.

Settings are stored in `%APPDATA%\AlwaysBindWindow\settings.json`; the old directory name is retained for compatibility. The manual detection shortcut is stored as `hotkey_bind`, and Alt controls are configured under `alt`. Preserve the other fields when editing the existing file.

### Build

The application targets Windows. The current functionality is primarily used on Windows 11; this change has not been fully verified on Windows 10 or other platforms.

```bash
git clone https://github.com/XR-stb/AlwaysBindWindow.git
cd AlwaysBindWindow
cargo build --release
```

The executable is `target/release/AltSnapGroup.exe`. Run tests with `cargo test`.

## License

MIT License — see [LICENSE](LICENSE) for details.

This project incorporates window controls from [Alt3rSnap](https://github.com/alvytsk/alt3rsnap) under its MIT license.