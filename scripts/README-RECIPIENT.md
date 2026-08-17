# native-devtools-mcp — 使用说明(收到二进制的人看这里)

这是一个 **MCP server**:它不是双击运行的 App,而是被 MCP 客户端(Claude Desktop / Claude Code / Cursor)在后台拉起来用的。按下面三步装好即可。

> 只支持 macOS / Windows。下面是 macOS 步骤,Windows 见最后一节。

## 最快:一键脚本(macOS)

把 `native-devtools-mcp` 二进制和 `install-recipient.sh` 放在**同一个文件夹**,然后:

```bash
./install-recipient.sh
```

脚本会自动:清除 Gatekeeper 隔离 → 加可执行权限 → 启动 setup 向导(检查权限 + 写客户端配置)。最后 **重启你的 MCP 客户端** 即可。

## 手动三步(macOS)

**1. 解除隔离**(自编译二进制没有 Apple 公证,macOS 默认会拦):

```bash
chmod +x ./native-devtools-mcp
xattr -dr com.apple.quarantine ./native-devtools-mcp
```

**2. 跑向导**(检查权限 + 自动探测并配置 Claude Desktop / Claude Code / Cursor):

```bash
./native-devtools-mcp setup
```

**3. 重启 MCP 客户端。**

## 关键权限坑

macOS 的 **辅助功能(Accessibility)** 和 **屏幕录制(Screen Recording)** 权限,要授给**拉起 server 的那个程序**(Claude Desktop / 终端 / Claude Code),**不是**授给二进制文件本身。

没授权的表现:点击静默失败、截图全黑。`setup` 会帮你打开对应的系统设置面板。

## 客户端没被自动识别时,手动加配置

用二进制的**绝对路径**:

```json
{
  "mcpServers": {
    "native-devtools": {
      "command": "/绝对路径/native-devtools-mcp"
    }
  }
}
```

配置文件位置:
- **Claude Desktop (macOS):** `~/Library/Application Support/Claude/claude_desktop_config.json`
- **Claude Code:** 项目里的 `.mcp.json`,或 `claude mcp add native-devtools /绝对路径/native-devtools-mcp`
- **Cursor / 其他:** 该客户端的 MCP 配置,`command` 形式相同

> Claude Code 想免去每次点击/截图都确认,可在 `.claude/settings.local.json` 加:
> `{ "permissions": { "allow": ["mcp__native-devtools__*"] } }`

## Windows

不需要清隔离那一步。直接:

```
native-devtools-mcp.exe setup
```

然后重启客户端。Claude Desktop 配置在 `%APPDATA%\Claude\claude_desktop_config.json`。
