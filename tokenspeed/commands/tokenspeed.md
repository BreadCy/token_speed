---
description: 查询模型生成速度统计（tokens/s、TTFT、输出 token 量），支持 ZCode/Codex/OpenCode/Claude Code
argument-hint: "[次数] [工具: zc|cx|oc|cc]"
---

帮用户查询模型生成速度（tokens/s）统计并回复。用户参数：$ARGUMENTS

步骤：

1. 在 Bash（Git Bash）中定位并运行插件自带的统计程序（单文件 exe，无需任何运行时）：

```bash
EXE=$(ls -1 /c/Users/czh/.zcode/cli/plugins/cache/*/tokenspeed/*/bin/tokenspeed.exe 2>/dev/null | sort -V | tail -1)
[ -z "$EXE" ] && EXE="/e/zcode_plugin/tokenspeed/bin/tokenspeed.exe"
"$EXE" --limit 10
```

- 若用户在参数中给了次数（如 `/tokenspeed 20`），把 `--limit` 的值换成该数字。
- 若用户只想看某个工具，追加 `--tool` 参数：`zc`=ZCode、`cx`=Codex、`oc`=OpenCode、`cc`=Claude Code（如 `/tokenspeed codex` → `--tool cx`）。
- 若用户想看指定 ZCode 会话，可用 `--session sess_xxx`。
- 若 exe 不存在，回退用 `py -3 E:/zcode_plugin/tokenspeed/scripts/tokenspeed.py --limit 10`（Python 后备版，仅支持 ZCode）。

2. 程序 stdout 包含：各工具最近一次速度、ZCode 会话汇总、最近若干次明细表（SRC 列标识来源工具：ZC=Codex 前的 ZCode、CX=Codex、OC=OpenCode、CC=Claude Code）。把它们原样或稍作排版回复给用户，不要编造或修改数字。
3. `~` 前缀表示该条速度按本地记录估算（含工具执行时间）；ZCode 数据为精确值。其余工具显示"（无本地会话数据）"属正常（如本机未用过 Claude Code）。
4. 若程序报错（如找不到数据库）或无数据，如实告知原因，不要猜测速度数值。
