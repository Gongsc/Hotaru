# Hotaru

Komari（[komari-monitor/komari](https://github.com/komari-monitor/komari)）的 macOS / Windows 托盘监控客户端。基于 Tauri 2（Rust + 系统 WebView），安装包小、常驻内存低。

![platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows-blue) ![license](https://img.shields.io/badge/license-GPL--3.0-green)

## v0.1.11 更新

- 新增左键面板失焦后保持打开的设置，置顶按钮独立控制窗口置顶。
- 记住主面板、设置窗口的大小和位置，以及左键面板的宽度和位置，重启后恢复。
- 左键面板支持横向调整宽度；展开、收起时高度随内容变化，达到屏幕可用高度后才滚动。
- 流量曲线随面板宽度重绘，节点与汇总曲线统一时间范围、纵轴范围及刻度。
- 修复 macOS 托盘点击处理，并确保关闭窗口后应用继续驻留托盘。

## 功能

- **主窗口内嵌 Komari 官方面板**：webview 直接加载你配置的后端地址，功能与浏览器一致；注入的自定义标题栏提供后退 / 前进 / 刷新与窗口控制；页面长时间加载不完会自动重载、必要时重建 webview（白屏自愈）；关闭窗口仅隐藏到托盘。
- **托盘图标实时状态**：单色“状态环 + 活动脉冲”——完整状态环 = 正常，右上圆点 = CPU/内存超过阈值或部分节点离线，斜杠 = 全部离线或后端不可达；macOS 使用 Template Image 自动反色，Windows 根据系统主题切换深浅前景。
  - 悬浮提示显示在线数、CPU、内存、实时上下行速率。
  - **macOS 菜单栏文字**：图标旁分两行显示实时速率，箭头成列、数值右对齐（`↑ 16 KB/s` / `↓ 5 KB/s`）；B/s 与 KB/s 取整，MB/s 起保留一位小数。Windows 因系统限制仅显示图标。
- **左键弹窗**：首次贴着托盘图标弹出，再次打开时恢复上次位置和宽度。
  - 顶部标题旁显示在线 / 离线 / 总节点数（覆盖全部节点，不受下方筛选影响）；下面是汇总上下行速率曲线，区间可选 5 分钟 / 15 分钟 / 1 小时 / 6 小时，悬停显示该时刻的时间与 ↑↓ 速率。
  - 下方按节点列出在线状态、地区与系统，名称后可选显示标签 / 延迟与丢包 / 网络速率 / 到期时间（设置里单选，默认标签；只改折叠行，标签筛选与搜索不受影响）；展开后显示 CPU / 内存 / 硬盘 / 流量（含限额）、运行时长与到期剩余、累计上传下载，以及最近 1 小时的网络质量（延迟与丢包，12 格 × 5 分钟）。
  - 汇总与列表之间有搜索框，右侧三个图标按钮依次弹出标签筛选、排序菜单与分组菜单：按名称或标签搜索，点标签只看带该标签的节点（多选取并集）；可按名称 / 系统 / CPU / 内存 / 网络速率 / 流量 / 延迟 / 丢包 / 到期时间排序，再点同一项换升降序，默认名称升序；分组为单选，默认「全部分组」，后端只给部分节点配了分组时菜单末尾还有「未分组」。Esc 分三级——先收浮层、再清筛选、最后才收起面板。
  - 弹窗开着时 macOS 菜单栏图标保持选中高亮，与原生菜单栏项一致。
  - 可按住标题栏拖动，只支持横向手动调整大小。默认失焦自动收起；设置中开启“左键面板失焦后保持打开”后，切换到其他窗口也会保留面板。点“置顶”会让面板保持最前且失焦不收起；取消置顶后，失焦行为遵循设置开关。
  - 高度由内容自动决定：展开节点时优先增高、收起时缩小，上限是锚定方向的屏幕可用空间（按工作区算，排除菜单栏、任务栏/Dock）；只有内容超过可用高度时节点列表才滚动，不显示滚动条。自动高度变化保留手动设置的宽度。
  - 汇总和节点流量曲线随可用宽度重绘，图表高度保持固定；使用统一的时间范围、纵轴范围、刻度和标签格式，便于比较。
- **窗口记忆**：主面板和设置窗口恢复上次大小和位置；左键面板恢复宽度和位置，高度按当前内容重新计算。跨屏或显示器变化后限制恢复位置，避免窗口落到屏幕外。
- **右键菜单**：打开面板、刷新面板、设置…、开机自启、退出。
- **配置**：设置窗口分为“连接 / 常规 / 关于”三页；后端地址、API Key、刷新间隔、CPU/内存告警阈值、曲线区间、节点名称后显示的内容、节点显示（勾选哪些节点进弹窗列表，支持全选/全部取消）、跟随系统/亮色/暗色主题、自签名证书容忍、开机自启、左键面板失焦后保持打开、静默启动（启动时不打开主面板，只驻留托盘）均在修改后自动保存。
- **检查更新**：设置窗口“关于”里比对 GitHub 最新 Release，可直接跳到下载页。

## 数据来源（Komari API）

| 用途 | 接口 |
|---|---|
| 实时快照 | 公开站点使用 WebSocket `/api/clients`（发送 `get` 一次拿回全部节点），失败时自动回退 HTTP；配置 API Key 时改用带 Bearer 的 `GET /api/nodes` + `GET /api/recent/{uuid}` 轮询（每轮最多 8 个请求并发），确保隐藏节点返回完整信息 |
| 节点名称/规格 | `GET /api/nodes`（每 60 秒刷新，设置里手动「获取节点列表」会立刻重读一次；节点标签取自其中的 `tags`，Komari 以 `;` 分隔） |
| 网络质量 | `GET /api/records/ping?uuid={uuid}&hours=1`（引擎每 60 秒在后台为全部节点刷新，最多 8 个并发；延迟与丢包随快照下发，展开节点即有，不再点开才请求） |
| 连接测试 | `GET /api/nodes` + `GET /api/version` |

- 公开站点无需认证即可使用；**私有站点**（开启 `private_site`）需要 API Key：管理后台 → 设置 → API Key（≥12 位），本应用以 `Authorization: Bearer <key>` 携带（API Key 同时绕过服务端 CORS/WS Origin 校验）。
- 实时数据粒度跟随 Komari Agent 上报间隔（默认 3 秒），应用刷新间隔默认 3 秒、可配 1–60 秒。

## 开发

依赖：Rust stable（Windows 需 MSVC，macOS 需 Xcode CLT）、WebView2（Win11 内置）。

```bash
# Windows / macOS 通用
cargo install tauri-cli --locked --version "^2"
cargo tauri dev      # 开发运行
cargo tauri build    # 产出安装包
```

设置窗口源码在 `ui/index.html`、托盘左键弹窗在 `ui/chart.html`（纯 HTML/JS，无前端构建步骤）；Rust 源码在 `src-tauri/src/`：

| 文件 | 职责 |
|---|---|
| `models.rs` | Komari 线上格式解析、快照聚合、图标状态机、格式化（含单元测试） |
| `monitor.rs` | WebSocket 实时会话 + HTTP 轮询降级、事件广播 |
| `tray.rs` | tiny-skia 动态图标绘制、托盘菜单、tooltip、macOS 菜单栏文字 |
| `windows.rs` | 面板窗口（加载后端 URL、注入标题栏、白屏看门狗）、弹窗定位与尺寸、设置窗口管理 |
| `window_geometry.rs` | 独立存储窗口位置与尺寸，恢复时适配显示器工作区 |
| `commands.rs` | Tauri 命令（读写设置、测试连接、ping 代理、检查更新、自启等） |
| `state.rs` | 运行时共享状态、内存中的网络历史（固定保留 6 小时） |

在仓库根目录运行检查：

```bash
cargo test --manifest-path src-tauri/Cargo.toml
node tools/test-chart.cjs
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
```

窗口与图表变更还需在 macOS / Windows 验证置顶、失焦、横向缩放、展开滚动和重启恢复；自动化测试不能替代原生窗口交互验证。

## 打包发布

推 tag `v*` 或手动触发 GitHub Actions（`.github/workflows/release.yml`），矩阵构建：

- macOS：`aarch64-apple-darwin`（Apple Silicon）与 `x86_64-apple-darwin`（Intel）→ `.dmg`
- Windows：`x86_64-pc-windows-msvc` → NSIS `.exe` 安装包

## 许可证

[GPL-3.0](LICENSE)

## 已知限制

- Windows 托盘无法显示文字速率（系统限制），信息见悬浮提示与左键弹窗。
- 速率曲线只存在内存里（固定保留 6 小时），退出应用即清空；换后端地址或 API Key 保存后也会立即清空并重新积累（旧站点的节点与曲线不会残留）；时间粒度跟随刷新间隔。
- 累计流量取 Komari 上报的 `network.total_up` / `total_down`，不是“当日”用量；网络质量依赖后端已配置 ping 监控任务。
