package main

import (
	"bufio"
	"bytes"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"go/ast"
	"go/format"
	"go/parser"
	"go/token"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
)

var uiArgumentIndexes = map[string][]int{
	"Text": {1}, "Textf": {1}, "Label": {0}, "LabelText": {0},
	"Tooltip": {0}, "Placeholder": {0}, "SettingLabel": {0},
	"StatusText": {0}, "StatusTextf": {0},
	"componentTextButton": {2}, "settingsRow": {1, 2},
	"settingsGroupCard": {1}, "settingsTextInput": {2, 5},
	"settingsSearchInput": {2}, "settingsChoice": {2},
	"settingsFormField": {2}, "formTitle": {1},
	"formSecretField": {2}, "formActions": {2},
}

var statusFields = map[string]bool{
	"status": true, "message": true, "err": true, "fatalErr": true,
	"detailErr": true, "fileErr": true,
}

type catalog map[string]string

func main() {
	apply := flag.Bool("apply", false, "write transformed Go files")
	dryRun := flag.Bool("dry-run", false, "report census and proposed changes")
	filesArg := flag.String("files", "", "comma-separated Go source files to process")
	localeDir := flag.String("locale-dir", "cmd/app/locales", "directory containing locale JSON catalogs")
	rustDir := flag.String("rust-locales", "../locales", "Rust locale directory used to fill known UI translations")
	catalogOnly := flag.Bool("catalog-only", false, "update catalogs without changing Go source files")
	check := flag.Bool("check", false, "fail when a collected string has no translation")
	flag.Parse()
	if *apply == *dryRun {
		fail(errors.New("choose exactly one of --dry-run or --apply"))
	}
	if strings.TrimSpace(*filesArg) == "" {
		fail(errors.New("--files must name the exact Go files to process"))
	}
	files := expandFiles(strings.Split(*filesArg, ","))
	seen := make(map[string]bool)
	texts := make(map[string]bool)
	changes := make(map[string][]byte)
	for _, file := range files {
		file = filepath.Clean(strings.TrimSpace(file))
		if file == "." || seen[file] {
			fail(fmt.Errorf("invalid or duplicate file %q", file))
		}
		seen[file] = true
		original, err := os.ReadFile(file)
		if err != nil {
			fail(err)
		}
		fset := token.NewFileSet()
		fileAST, err := parser.ParseFile(fset, file, original, parser.ParseComments)
		if err != nil {
			fail(err)
		}
		ast.Inspect(fileAST, func(node ast.Node) bool {
			if function, ok := node.(*ast.FuncDecl); ok && function.Body != nil {
				render := false
				for _, parameter := range function.Type.Params.List {
					if pointer, ok := parameter.Type.(*ast.StarExpr); ok {
						if selector, ok := pointer.X.(*ast.SelectorExpr); ok && calledName(selector.X) == "ui" && selector.Sel.Name == "Context" {
							render = true
						}
					}
				}
				labelResult := map[string]bool{"accessLabel": true, "surfaceLabel": true, "surfaceBaseLabel": true}[function.Name.Name]
				ast.Inspect(function.Body, func(child ast.Node) bool {
					if assignment, ok := child.(*ast.AssignStmt); ok && render {
						for index, lhs := range assignment.Lhs {
							if index >= len(assignment.Rhs) {
								break
							}
							if identifier, ok := lhs.(*ast.Ident); ok && (identifier.Name == "label" || identifier.Name == "title" || identifier.Name == "text" || identifier.Name == "description" || identifier.Name == "message" || identifier.Name == "status" || identifier.Name == "toggle") {
								assignment.Rhs[index] = translateUIExpression(assignment.Rhs[index], texts)
							}
						}
					}
					if returned, ok := child.(*ast.ReturnStmt); ok && labelResult && len(returned.Results) > 0 {
						returned.Results[0] = translateUIExpression(returned.Results[0], texts)
					}
					return true
				})
			}
			if composite, ok := node.(*ast.CompositeLit); ok {
				collectOptionComposite(composite, texts)
			}
			call, ok := node.(*ast.CallExpr)
			if ok && len(call.Args) != 0 {
				name := calledName(call.Fun)
				indexes := uiArgumentIndexes[name]
				wrapper := "tr"
				for _, i := range indexes {
					if i >= len(call.Args) {
						continue
					}
					arg := call.Args[i]
					call.Args[i] = translateUIExpression(arg, texts)
					literal, translated := translationLiteral(arg)
					if literal == nil {
						continue
					}
					text, err := strconv.Unquote(literal.Value)
					if err != nil || !isHumanText(text) {
						continue
					}
					texts[text] = true
					if !translated {
						call.Args[i] = &ast.CallExpr{Fun: ast.NewIdent(wrapper), Args: []ast.Expr{literal}}
					}
				}
			}
			if !ok {
				if assignment, ok := node.(*ast.AssignStmt); ok {
					for i, lhs := range assignment.Lhs {
						if i >= len(assignment.Rhs) {
							break
						}
						selector, ok := lhs.(*ast.SelectorExpr)
						if !ok || !statusFields[selector.Sel.Name] {
							continue
						}
						assignment.Rhs[i] = translateUIExpression(assignment.Rhs[i], texts)
					}
				}
			}
			return true
		})
		var output bytes.Buffer
		if err := format.Node(&output, fset, fileAST); err != nil {
			fail(err)
		}
		changes[file] = output.Bytes()
	}

	catalogs := loadCatalogs(*localeDir)
	rustEnglish, rustChinese := readRustCatalog(filepath.Join(*rustDir, "app.yml"), filepath.Join(*rustDir, "zh-CN.yml"))
	_, rustJapanese := readRustCatalog(filepath.Join(*rustDir, "app.yml"), filepath.Join(*rustDir, "ja.yml"))
	rustKeyByEnglish := make(map[string]string, len(rustEnglish))
	ambiguousEnglish := make(map[string]bool)
	for key, english := range rustEnglish {
		if previous, exists := rustKeyByEnglish[english]; exists && previous != key {
			ambiguousEnglish[english] = true
			continue
		}
		rustKeyByEnglish[english] = key
	}
	for english := range ambiguousEnglish {
		delete(rustKeyByEnglish, english)
	}
	for text := range texts {
		if catalogs["en"][text] == "" {
			catalogs["en"][text] = text
		}
		if catalogs["zh-CN"][text] == "" {
			catalogs["zh-CN"][text] = rustChinese[rustKeyByEnglish[text]]
		}
		if catalogs["ja"][text] == "" {
			catalogs["ja"][text] = rustJapanese[rustKeyByEnglish[text]]
		}
		if translation, ok := manualTranslations["zh-CN"][text]; ok {
			catalogs["zh-CN"][text] = translation
		}
		if translation, ok := manualTranslations["ja"][text]; ok {
			catalogs["ja"][text] = translation
		}
	}
	for english, key := range rustKeyByEnglish {
		if catalogs["en"][english] == "" && rustChinese[key] != "" && rustJapanese[key] != "" {
			catalogs["en"][english], catalogs["zh-CN"][english], catalogs["ja"][english] = english, rustChinese[key], rustJapanese[key]
		}
	}
	missing := false
	for _, entry := range extraTranslations {
		catalogs["en"][entry[0]], catalogs["zh-CN"][entry[0]], catalogs["ja"][entry[0]] = entry[0], entry[1], entry[2]
	}
	for _, text := range sortedKeys(texts) {
		fmt.Printf("UI\t%s\n", text)
		for _, language := range []string{"zh-CN", "ja"} {
			if catalogs[language][text] == "" {
				fmt.Printf("MISSING\t%s\t%s\n", language, text)
				missing = true
			}
		}
	}
	if *check && missing {
		fail(errors.New("one or more UI strings have no translation"))
	}
	if *dryRun {
		if *catalogOnly {
			return
		}
		for file, content := range changes {
			original, _ := os.ReadFile(file)
			if !bytes.Equal(original, content) {
				fmt.Printf("WOULD_UPDATE\t%s\n", file)
			}
		}
		return
	}
	if !*catalogOnly {
		for file, content := range changes {
			if err := os.WriteFile(file, content, 0o644); err != nil {
				fail(err)
			}
		}
	}
	for _, language := range []string{"en", "zh-CN", "ja"} {
		if catalogs[language] == nil {
			catalogs[language] = catalog{}
		}
		if err := writeCatalog(filepath.Join(*localeDir, language+".json"), catalogs[language]); err != nil {
			fail(err)
		}
	}
}

var manualTranslations = map[string]catalog{
	"zh-CN": {
		"A fork is already running": "分支任务已在运行", "A previous submission is unconfirmed. Check the session before sending again.": "上一条提交尚未确认。请先检查会话，再重新发送。", "A steer is unconfirmed. Check the session before sending again.": "上一条引导消息尚未确认。请先检查会话，再重新发送。",
		"Awaiting sign-in": "等待登录", "Browser tools are not enabled": "未启用浏览器工具", "Cannot decode the reconciled provider transcript": "无法解析已核对的提供商对话记录", "Cannot steer while the agent runtime is unavailable": "代理运行时不可用时无法发送引导消息", "Checking saved history": "正在检查已保存的历史记录",
		"Configuration changed elsewhere. Your draft is kept; save again to apply it.": "配置已在其他位置更改。草稿已保留，请再次保存以应用。", "Configuration reloaded. Your draft is kept; save to apply it.": "配置已重新加载。草稿已保留，请保存以应用。", "Connect to the daemon before changing this task": "请先连接守护进程，再更改此任务", "Context compaction requested": "已请求压缩上下文", "Context compaction was not confirmed. It will not be resent automatically.": "上下文压缩尚未确认。不会自动重新发送。",
		"Could not decode daemon settings": "无法解析守护进程设置", "Could not open the file picker": "无法打开文件选择器", "Credential saved. Account status is refreshing.": "凭据已保存。正在刷新账户状态。", "Deletion completed on the previous connection. Reload tasks to check.": "删除已在上一次连接中完成。请重新加载任务以确认。", "Desktop title save queued": "桌面端标题保存已排队", "Desktop title save was not applied because the task or connection changed. Check the provider title.": "任务或连接已更改，桌面端标题未能应用。请检查提供商标题。",
		"Edit the one-time schedule to a future time before enabling it.": "请先将单次计划修改为未来时间，再启用。", "Enter a shell executable path": "输入 Shell 可执行文件路径", "Fork result belongs to a previous task or connection. Check tasks before forking again.": "分支结果属于之前的任务或连接。再次创建分支前请检查任务。", "History restored. The edited message is in the queue for explicit sending.": "历史记录已恢复。编辑后的消息已排队，需手动发送。", "Invalid settings response": "设置响应无效", "Loading sessions": "正在加载会话",
		"No local shell for this session": "此会话没有本地 Shell", "Not saved. Local preferences are unavailable": "未保存。本地偏好设置不可用", "Not saved. Settings writer is busy": "未保存。设置写入器正忙", "OpenCode updated": "OpenCode 已更新", "Provider title updated; desktop save queued": "提供商标题已更新，桌面端保存已排队", "Proxy port must be between 1 and 65535": "代理端口必须介于 1 到 65535 之间", "Reconnecting to the daemon": "正在重新连接守护进程",
		"Reload settings before changing another value": "更改其他值前，请先重新加载设置", "Remote shell needs its target daemon connection, which is not implemented": "远程 Shell 需要连接目标守护进程，此功能尚未实现", "Resolve the pending submission before changing this task": "更改此任务前，请先处理待确认的提交", "Resolve the pending task operation before sending": "发送前，请先处理待完成的任务操作", "Reverting history and file changes": "正在撤回历史记录和文件更改", "Rewind completed on the previous task or connection. Reload history before continuing.": "回退已在上一个任务或连接中完成。继续前请重新加载历史记录。", "Rewind is still unconfirmed. No message was resent.": "回退仍未确认。未重新发送任何消息。",
		"Saved": "已保存", "Saved settings confirmed": "已确认设置保存成功", "Saved values differ from the draft. Reload before retrying.": "已保存的值与草稿不同。重试前请重新加载。", "Saving...": "正在保存…", "Settings write cancelled": "设置写入已取消", "Starting the daemon": "正在启动守护进程", "Stop the task before changing it": "更改任务前请先停止任务", "Task deleted": "任务已删除", "Task forked": "已创建任务分支", "Task no longer exists. Close the draft or create a different task.": "任务已不存在。请关闭草稿或创建其他任务。",
		"The daemon is not connected": "守护进程未连接", "The daemon is shutting down": "守护进程正在关闭", "The daemon restarted. Reopen the app to restore saved sessions.": "守护进程已重启。请重新打开应用以恢复已保存的会话。", "The edited message is empty": "编辑后的消息为空", "The provider transcript could not replace this turn safely": "无法安全地用提供商对话记录替换此轮内容", "The rewound task identity differs. Reload history before continuing.": "回退后的任务身份不一致。继续前请重新加载历史记录。", "The steer was not delivered": "引导消息未送达", "The task is active. No message was resent.": "任务仍在运行。未重新发送消息。", "The task or connection changed. Reload history before continuing.": "任务或连接已更改。继续前请重新加载历史记录。", "The task or connection changed. Select Delete again.": "任务或连接已更改。请再次选择删除。", "The workspace changed. Select Delete again.": "工作区已更改。请再次选择删除。", "This setting is not supported by the daemon": "守护进程不支持此设置", "This task's workspace is unavailable on this device": "此设备无法访问该任务的工作区", "Threshold must be between 10000 and 100000000 tokens": "阈值必须介于 10000 到 100000000 个令牌之间",
		"Use an HTTP or HTTPS proxy with host and port, without credentials": "请使用包含主机和端口且不含凭据的 HTTP 或 HTTPS 代理", "Wait for pending saves before deleting this task": "删除此任务前，请等待待处理的保存完成", "Wait for session options to finish saving": "请等待会话选项保存完成", "Wait for the attachments to finish importing": "请等待附件导入完成", "Waiting for pending saves": "正在等待待处理的保存", "Windows rejected the wake request": "Windows 拒绝了保持唤醒请求", "Workspace changed. Refresh before another write.": "工作区已更改。再次写入前请刷新。", "Workspace path is unavailable": "工作区路径不可用",
		"API key": "API 密钥", "Applied to update checks, the model catalog, and new sessions": "应用于更新检查、模型目录和新会话", "Authorization code": "授权码",
		"Automatic uses light from 07:00 to 19:00 local time": "本地时间 07:00 至 19:00 使用浅色主题", "Available image models": "可用的图像模型", "Back to range": "返回范围设置",
		"Choose executable": "选择可执行文件", "Choose folder": "选择文件夹", "Choose the font used for code, diffs, the file pane, and the terminal.": "选择代码、差异、文件面板和终端使用的字体。", "Choose the font used throughout the app interface": "选择应用界面使用的字体",
		"Color scheme": "配色方案", "Command arguments (JSON)": "命令参数（JSON）", "Configuration scope": "配置范围", "Delete conversations": "删除会话", "Delete task": "删除任务", "Discard draft": "放弃草稿",
		"Environment (JSON)": "环境变量（JSON）", "Headers (JSON)": "请求头（JSON）", "Image model": "图像模型", "Interface scale": "界面缩放", "Load accounts": "加载账户", "Load models": "加载模型", "Model ID": "模型 ID",
		"Move to Trash": "移到回收站", "Not available in this build": "此版本不可用", "OAuth": "OAuth", "OAuth client ID": "OAuth 客户端 ID", "OAuth scope": "OAuth 权限范围", "Open sign-in page": "打开登录页面", "OpenCode configuration": "OpenCode 配置",
		"Preview eligible inactive conversations before deletion": "删除前预览符合条件的非活动会话", "Probe models": "探测模型", "Reload configuration": "重新加载配置", "Reload history": "重新加载历史记录", "Reload settings": "重新加载设置", "Remove provider": "移除提供商", "Remove server": "移除服务器", "Retention": "保留期限",
		"Retry pending saves": "重试待处理的保存", "Revert and send": "撤回并发送", "Save anyway": "仍然保存", "Save configuration": "保存配置", "Scale the app interface": "缩放应用界面", "Search marketplace": "搜索市场", "Search servers": "搜索服务器", "Search skills": "搜索技能", "Select": "选择", "Server type": "服务器类型",
		"Session cleanup": "会话清理", "Sign in with OAuth": "使用 OAuth 登录", "Skill source": "技能来源", "Transcript width": "对话记录宽度", "Use selected account": "使用所选账户", "View day": "查看日期",
		"Revert this turn and all later turns, including their file changes, then send the edited message?": "撤回此轮及后续所有轮次（包括文件更改），然后发送已编辑的消息？",
		" Needs auth": "需要认证", "?": "?", "Access scope": "访问范围", "Another answer": "其他回答", "Attach files": "附加文件",
		"Access is limited to workspaces registered when pairing starts; it cannot add workspaces, browse arbitrary directories, or change global settings.": "访问范围仅限于配对时登记的工作区。无法添加工作区、浏览任意目录或更改全局设置。",
		"Authentication may require OAuth or provider headers. Configure them in MCP Servers.":                                                               "认证可能需要 OAuth 或提供商请求头。请在 MCP 服务器中配置。",
		"Built-in · managed by the OpenCode CLI": "内置，由 OpenCode CLI 管理", "Clear search": "清除搜索", "Close command palette": "关闭命令面板", "Command palette": "命令面板",
		"Daily activity calendar": "每日活动日历", "Daily token usage": "每日令牌用量", "Delete task?": "删除任务？", "Desktop bridge unavailable": "桌面桥接不可用",
		"Enter a path, or Browse for a folder": "输入路径，或浏览选择文件夹", "Esc": "Esc", "Headers-only authentication": "仅请求头认证", "Input + generated, excluding cache": "输入与生成用量，不含缓存",
		"Loading": "加载中", "Loading connected accounts...": "正在加载已连接的账户…", "Mobile remote": "手机远程控制", "Next chart day": "图表下一天", "No content": "无内容", "No direct token usage in this period": "此期间没有直接令牌用量",
		"No quota reported": "未报告配额", "No recorded usage": "没有已记录的用量", "No settings found": "未找到设置", "No workspace connected": "未连接工作区",
		"Once paired, your phone can manage sessions, send messages and attachments, stop tasks, and handle approvals and questions.": "配对后，你可以用手机管理会话、发送消息和附件、停止任务，以及处理授权和问题。",
		"Open a project to browse its files.": "打开项目以浏览文件。", "Open project folder": "打开项目文件夹", "Open usage": "打开用量页面", "OpenCode": "OpenCode", "OpenCode session store": "OpenCode 会话存储", "Pairing": "配对",
		"Pairing scope: registered workspaces and sessions updated within 72 hours.": "配对范围：已登记的工作区，以及过去 72 小时内更新的会话。", "Preview only. No access granted.": "仅供预览。未授予访问权限。", "Previous chart day": "图表前一天", "Project folder": "项目文件夹", "Prompt to run": "运行提示词", "Provider accounts": "提供商账户",
		"Quota data unavailable": "配额数据不可用", "Refreshing...": "正在刷新…", "Remove attachment": "移除附件", "Remove model": "移除模型", "Remove this MCP server configuration?": "移除此 MCP 服务器配置？", "Remove this provider configuration?": "移除此提供商配置？", "Rename session": "重命名会话", "SOURCE": "来源",
		"Image generation": "图像生成", "Todo list": "任务清单",
		"Save would overwrite another change. Pick which copy wins.": "保存会覆盖另一项更改。请选择要保留的版本。", "Saving": "正在保存", "Scheduled task list": "定时任务列表", "Search models": "搜索模型", "Select a task": "选择任务", "Start pairing": "开始配对", "Subscriptions & balances": "订阅与余额", "Switch branch": "切换分支", "Task prompt": "任务提示词", "The file changed on disk": "文件已在磁盘上更改",
		"This also deletes its linked provider conversation. Project files are kept.": "这也会删除关联的提供商会话。项目文件会保留。", "This desktop build does not implement the remote bridge.": "此桌面版本尚未实现远程桥接。", "Try a task title, project, setting, or command": "试试搜索任务标题、项目、设置或命令", "Type a command…": "输入命令…", "Unstage or resolve this rename before discarding": "请先取消暂存或解决此重命名，再执行丢弃", "Usage date range": "用量日期范围", "What should we build in": "要在哪个项目中构建",
	},
	"ja": {
		"A fork is already running": "フォークタスクはすでに実行中です", "A previous submission is unconfirmed. Check the session before sending again.": "前回の送信は未確認です。再送信する前にセッションを確認してください。", "A steer is unconfirmed. Check the session before sending again.": "前回の誘導メッセージは未確認です。再送信する前にセッションを確認してください。",
		"Awaiting sign-in": "サインイン待ち", "Browser tools are not enabled": "ブラウザツールが有効になっていません", "Cannot decode the reconciled provider transcript": "照合済みのプロバイダー会話を解析できません", "Cannot steer while the agent runtime is unavailable": "エージェントのランタイムが利用できないため誘導できません", "Checking saved history": "保存済みの履歴を確認中",
		"Configuration changed elsewhere. Your draft is kept; save again to apply it.": "設定が別の場所で変更されました。下書きは保持されています。適用するにはもう一度保存してください。", "Configuration reloaded. Your draft is kept; save to apply it.": "設定を再読み込みしました。下書きは保持されています。適用するには保存してください。", "Connect to the daemon before changing this task": "このタスクを変更する前にデーモンへ接続してください", "Context compaction requested": "コンテキストの圧縮を要求しました", "Context compaction was not confirmed. It will not be resent automatically.": "コンテキストの圧縮は確認されていません。自動では再送信されません。",
		"Could not decode daemon settings": "デーモン設定を解析できませんでした", "Could not open the file picker": "ファイル選択画面を開けませんでした", "Credential saved. Account status is refreshing.": "認証情報を保存しました。アカウントの状態を更新中です。", "Deletion completed on the previous connection. Reload tasks to check.": "前回の接続で削除が完了しました。タスクを再読み込みして確認してください。", "Desktop title save queued": "デスクトップのタイトル保存をキューに追加しました", "Desktop title save was not applied because the task or connection changed. Check the provider title.": "タスクまたは接続が変更されたため、デスクトップのタイトルを適用できませんでした。プロバイダーのタイトルを確認してください。",
		"Edit the one-time schedule to a future time before enabling it.": "有効にする前に、1回限りのスケジュールを未来の時刻に変更してください。", "Enter a shell executable path": "シェル実行ファイルのパスを入力", "Fork result belongs to a previous task or connection. Check tasks before forking again.": "フォーク結果は以前のタスクまたは接続のものです。再度フォークする前にタスクを確認してください。", "History restored. The edited message is in the queue for explicit sending.": "履歴を復元しました。編集したメッセージは手動送信用のキューにあります。", "Invalid settings response": "設定の応答が無効です", "Loading sessions": "セッションを読み込み中",
		"No local shell for this session": "このセッションにはローカルシェルがありません", "Not saved. Local preferences are unavailable": "未保存です。ローカル設定を利用できません", "Not saved. Settings writer is busy": "未保存です。設定の書き込み中です", "OpenCode updated": "OpenCode を更新しました", "Provider title updated; desktop save queued": "プロバイダーのタイトルを更新し、デスクトップへの保存をキューに追加しました", "Proxy port must be between 1 and 65535": "プロキシポートは 1〜65535 にしてください", "Reconnecting to the daemon": "デーモンに再接続中",
		"Reload settings before changing another value": "別の値を変更する前に設定を再読み込みしてください", "Remote shell needs its target daemon connection, which is not implemented": "リモートシェルには接続先デーモンが必要ですが、この機能は未実装です", "Resolve the pending submission before changing this task": "このタスクを変更する前に保留中の送信を解決してください", "Resolve the pending task operation before sending": "送信する前に保留中のタスク操作を解決してください", "Reverting history and file changes": "履歴とファイル変更を取り消しています", "Rewind completed on the previous task or connection. Reload history before continuing.": "前のタスクまたは接続で巻き戻しが完了しました。続行する前に履歴を再読み込みしてください。", "Rewind is still unconfirmed. No message was resent.": "巻き戻しは未確認です。メッセージは再送信されていません。",
		"Saved": "保存しました", "Saved settings confirmed": "設定の保存を確認しました", "Saved values differ from the draft. Reload before retrying.": "保存済みの値が下書きと異なります。再試行する前に再読み込みしてください。", "Saving...": "保存中…", "Settings write cancelled": "設定の書き込みをキャンセルしました", "Starting the daemon": "デーモンを起動中", "Stop the task before changing it": "変更する前にタスクを停止してください", "Task deleted": "タスクを削除しました", "Task forked": "タスクをフォークしました", "Task no longer exists. Close the draft or create a different task.": "タスクは存在しません。下書きを閉じるか、別のタスクを作成してください。",
		"The daemon is not connected": "デーモンに接続されていません", "The daemon is shutting down": "デーモンを終了しています", "The daemon restarted. Reopen the app to restore saved sessions.": "デーモンが再起動しました。保存済みセッションを復元するにはアプリを再度開いてください。", "The edited message is empty": "編集したメッセージが空です", "The provider transcript could not replace this turn safely": "プロバイダーの会話でこのターンを安全に置き換えられませんでした", "The rewound task identity differs. Reload history before continuing.": "巻き戻したタスクの識別情報が異なります。続行する前に履歴を再読み込みしてください。", "The steer was not delivered": "誘導メッセージを配信できませんでした", "The task is active. No message was resent.": "タスクは実行中です。メッセージは再送信されていません。", "The task or connection changed. Reload history before continuing.": "タスクまたは接続が変更されました。続行する前に履歴を再読み込みしてください。", "The task or connection changed. Select Delete again.": "タスクまたは接続が変更されました。もう一度削除を選択してください。", "The workspace changed. Select Delete again.": "ワークスペースが変更されました。もう一度削除を選択してください。", "This setting is not supported by the daemon": "この設定はデーモンでサポートされていません", "This task's workspace is unavailable on this device": "このデバイスではタスクのワークスペースを利用できません", "Threshold must be between 10000 and 100000000 tokens": "しきい値は 10000〜100000000 トークンにしてください",
		"Use an HTTP or HTTPS proxy with host and port, without credentials": "認証情報を含まない、ホストとポートを指定した HTTP または HTTPS プロキシを使用してください", "Wait for pending saves before deleting this task": "このタスクを削除する前に保存の完了を待ってください", "Wait for session options to finish saving": "セッションオプションの保存完了を待ってください", "Wait for the attachments to finish importing": "添付ファイルのインポート完了を待ってください", "Waiting for pending saves": "保留中の保存を待機中", "Windows rejected the wake request": "Windows がスリープ抑止要求を拒否しました", "Workspace changed. Refresh before another write.": "ワークスペースが変更されました。再度書き込む前に更新してください。", "Workspace path is unavailable": "ワークスペースのパスを利用できません",
		"API key": "API キー", "Applied to update checks, the model catalog, and new sessions": "更新チェック、モデルカタログ、新しいセッションに適用", "Authorization code": "認証コード",
		"Automatic uses light from 07:00 to 19:00 local time": "現地時間 07:00〜19:00 はライトテーマを使用", "Available image models": "利用可能な画像モデル", "Back to range": "範囲設定に戻る",
		"Choose executable": "実行ファイルを選択", "Choose folder": "フォルダーを選択", "Choose the font used for code, diffs, the file pane, and the terminal.": "コード、差分、ファイルペイン、ターミナルに使うフォントを選択します。", "Choose the font used throughout the app interface": "アプリ全体で使うフォントを選択します",
		"Color scheme": "配色", "Command arguments (JSON)": "コマンド引数（JSON）", "Configuration scope": "設定の適用範囲", "Delete conversations": "会話を削除", "Delete task": "タスクを削除", "Discard draft": "下書きを破棄",
		"Environment (JSON)": "環境変数（JSON）", "Headers (JSON)": "ヘッダー（JSON）", "Image model": "画像モデル", "Interface scale": "インターフェースの倍率", "Load accounts": "アカウントを読み込む", "Load models": "モデルを読み込む", "Model ID": "モデル ID",
		"Move to Trash": "ゴミ箱に移動", "Not available in this build": "このビルドでは利用できません", "OAuth": "OAuth", "OAuth client ID": "OAuth クライアント ID", "OAuth scope": "OAuth スコープ", "Open sign-in page": "サインインページを開く", "OpenCode configuration": "OpenCode の設定",
		"Preview eligible inactive conversations before deletion": "削除前に対象となる非アクティブな会話をプレビュー", "Probe models": "モデルを確認", "Reload configuration": "設定を再読み込み", "Reload history": "履歴を再読み込み", "Reload settings": "設定を再読み込み", "Remove provider": "プロバイダーを削除", "Remove server": "サーバーを削除", "Retention": "保持期間",
		"Retry pending saves": "保留中の保存を再試行", "Revert and send": "取り消して送信", "Save anyway": "そのまま保存", "Save configuration": "設定を保存", "Scale the app interface": "アプリの表示倍率", "Search marketplace": "マーケットプレイスを検索", "Search servers": "サーバーを検索", "Search skills": "スキルを検索", "Select": "選択", "Server type": "サーバーの種類",
		"Session cleanup": "セッションの整理", "Sign in with OAuth": "OAuth でサインイン", "Skill source": "スキルのソース", "Transcript width": "会話欄の幅", "Use selected account": "選択したアカウントを使用", "View day": "日付を表示",
		"Revert this turn and all later turns, including their file changes, then send the edited message?": "このターンと後続のすべてのターン（ファイル変更を含む）を取り消して、編集したメッセージを送信しますか？",
		" Needs auth": "認証が必要", "?": "?", "Access scope": "アクセス範囲", "Another answer": "別の回答", "Attach files": "ファイルを添付",
		"Access is limited to workspaces registered when pairing starts; it cannot add workspaces, browse arbitrary directories, or change global settings.": "アクセスできるのはペアリング開始時に登録されたワークスペースのみです。ワークスペースの追加、任意のディレクトリの閲覧、全体設定の変更はできません。",
		"Authentication may require OAuth or provider headers. Configure them in MCP Servers.":                                                               "認証には OAuth またはプロバイダーのヘッダーが必要な場合があります。MCP サーバーで設定してください。",
		"Built-in · managed by the OpenCode CLI": "組み込み · OpenCode CLI で管理", "Clear search": "検索をクリア", "Close command palette": "コマンドパレットを閉じる", "Command palette": "コマンドパレット",
		"Daily activity calendar": "日別アクティビティカレンダー", "Daily token usage": "日別トークン使用量", "Delete task?": "タスクを削除しますか？", "Desktop bridge unavailable": "デスクトップブリッジを利用できません",
		"Enter a path, or Browse for a folder": "パスを入力するか、フォルダーを参照してください", "Esc": "Esc", "Headers-only authentication": "ヘッダーのみの認証", "Input + generated, excluding cache": "入力と生成。キャッシュを除く",
		"Loading": "読み込み中", "Loading connected accounts...": "接続済みアカウントを読み込み中…", "Mobile remote": "モバイルリモート", "Next chart day": "グラフの翌日", "No content": "コンテンツがありません", "No direct token usage in this period": "この期間に直接のトークン使用量はありません",
		"No quota reported": "クォータ情報がありません", "No recorded usage": "使用履歴がありません", "No settings found": "設定が見つかりません", "No workspace connected": "ワークスペースが接続されていません",
		"Once paired, your phone can manage sessions, send messages and attachments, stop tasks, and handle approvals and questions.": "ペアリングすると、スマートフォンでセッション管理、メッセージや添付ファイルの送信、タスクの停止、承認や質問への対応ができます。",
		"Open a project to browse its files.": "プロジェクトを開いてファイルを表示します。", "Open project folder": "プロジェクトフォルダーを開く", "Open usage": "使用状況を開く", "OpenCode": "OpenCode", "OpenCode session store": "OpenCode セッションストア", "Pairing": "ペアリング",
		"Pairing scope: registered workspaces and sessions updated within 72 hours.": "ペアリング範囲：登録済みワークスペースと、過去 72 時間以内に更新されたセッション。", "Preview only. No access granted.": "プレビューのみ。アクセス権は付与されていません。", "Previous chart day": "グラフの前日", "Project folder": "プロジェクトフォルダー", "Prompt to run": "実行するプロンプト", "Provider accounts": "プロバイダーアカウント",
		"Quota data unavailable": "クォータ情報を利用できません", "Refreshing...": "更新中…", "Remove attachment": "添付ファイルを削除", "Remove model": "モデルを削除", "Remove this MCP server configuration?": "この MCP サーバー設定を削除しますか？", "Remove this provider configuration?": "このプロバイダー設定を削除しますか？", "Rename session": "セッション名を変更", "SOURCE": "ソース",
		"Image generation": "画像生成", "Todo list": "タスクリスト",
		"Save would overwrite another change. Pick which copy wins.": "保存すると別の変更が上書きされます。残す内容を選択してください。", "Saving": "保存中", "Scheduled task list": "スケジュール済みタスク一覧", "Search models": "モデルを検索", "Select a task": "タスクを選択", "Start pairing": "ペアリングを開始", "Subscriptions & balances": "サブスクリプションと残高", "Switch branch": "ブランチを切り替え", "Task prompt": "タスクのプロンプト", "The file changed on disk": "ファイルがディスク上で変更されました",
		"This also deletes its linked provider conversation. Project files are kept.": "関連するプロバイダーの会話も削除されます。プロジェクトファイルは保持されます。", "This desktop build does not implement the remote bridge.": "このデスクトップ版ではリモートブリッジを実装していません。", "Try a task title, project, setting, or command": "タスク名、プロジェクト、設定、コマンドを検索してください", "Type a command…": "コマンドを入力…", "Unstage or resolve this rename before discarding": "破棄する前に、この名前変更のステージを解除するか解決してください", "Usage date range": "使用状況の日付範囲", "What should we build in": "どのプロジェクトで作業しますか",
	},
}

func isHumanText(text string) bool {
	text = strings.TrimSpace(text)
	if text == "" || text == "..." || text == "…" || strings.ContainsAny(text, "\r\n") {
		return false
	}
	if _, err := strconv.ParseFloat(text, 64); err == nil {
		return false
	}
	if strings.ContainsAny(text, "_/") || (strings.Contains(text, "-") && !strings.Contains(text, " ")) {
		return false
	}
	if strings.Contains(text, "{{") || strings.Contains(text, "%!") || strings.HasPrefix(text, "{\"") {
		return false
	}
	return true
}

func expandFiles(paths []string) []string {
	var files []string
	for _, path := range paths {
		path = filepath.Clean(strings.TrimSpace(path))
		info, err := os.Stat(path)
		if err != nil {
			fail(err)
		}
		if !info.IsDir() {
			files = append(files, path)
			continue
		}
		entries, err := os.ReadDir(path)
		if err != nil {
			fail(err)
		}
		for _, entry := range entries {
			name := entry.Name()
			if !entry.IsDir() && strings.HasSuffix(name, ".go") &&
				!strings.HasSuffix(name, "_e2e.go") && !strings.HasSuffix(name, "_e2e_test.go") &&
				!strings.HasPrefix(name, "i18n") {
				files = append(files, filepath.Join(path, entry.Name()))
			}
		}
	}
	return files
}

func calledName(expr ast.Expr) string {
	switch value := expr.(type) {
	case *ast.Ident:
		return value.Name
	case *ast.SelectorExpr:
		return value.Sel.Name
	}
	return ""
}

func translationLiteral(expr ast.Expr) (*ast.BasicLit, bool) {
	if literal, ok := expr.(*ast.BasicLit); ok && literal.Kind == token.STRING {
		return literal, false
	}
	call, ok := expr.(*ast.CallExpr)
	if !ok || (calledName(call.Fun) != "tr" && calledName(call.Fun) != "trf") || len(call.Args) == 0 {
		return nil, false
	}
	literal, ok := call.Args[0].(*ast.BasicLit)
	return literal, ok && literal.Kind == token.STRING
}

func collectOptionComposite(composite *ast.CompositeLit, texts map[string]bool) {
	if array, ok := composite.Type.(*ast.ArrayType); ok && (calledName(array.Elt) == "settingsOption" || calledName(array.Elt) == "settingsPage") {
		for _, element := range composite.Elts {
			if child, ok := element.(*ast.CompositeLit); ok {
				collectOptionLabels(child, calledName(array.Elt), texts)
			}
		}
		return
	}
	if calledName(composite.Type) != "settingsOption" && calledName(composite.Type) != "settingsPage" {
		return
	}
	collectOptionLabels(composite, calledName(composite.Type), texts)
}

func collectOptionLabels(composite *ast.CompositeLit, kind string, texts map[string]bool) {
	if len(composite.Elts) == 0 {
		return
	}
	index := 0
	if kind == "settingsPage" {
		index = 1
	}
	if index >= len(composite.Elts) {
		return
	}
	expression := composite.Elts[index]
	for _, element := range composite.Elts {
		if keyed, ok := element.(*ast.KeyValueExpr); ok && calledName(keyed.Key) == "label" {
			expression = keyed.Value
			break
		}
	}
	label, ok := expression.(*ast.BasicLit)
	if !ok || label.Kind != token.STRING {
		return
	}
	text, err := strconv.Unquote(label.Value)
	if err == nil && isHumanText(text) {
		texts[text] = true
	}
}

func translateUIExpression(expression ast.Expr, texts map[string]bool) ast.Expr {
	if literal, translated := translationLiteral(expression); literal != nil {
		text, err := strconv.Unquote(literal.Value)
		if err == nil && isHumanText(text) {
			texts[text] = true
			if !translated {
				return &ast.CallExpr{Fun: ast.NewIdent("tr"), Args: []ast.Expr{literal}}
			}
		}
		return expression
	}
	switch value := expression.(type) {
	case *ast.BinaryExpr:
		if value.Op == token.ADD {
			value.X = translateUIExpression(value.X, texts)
			value.Y = translateUIExpression(value.Y, texts)
		}
	case *ast.CallExpr:
		name := calledName(value.Fun)
		if name == "Sprintf" && len(value.Args) > 0 {
			value.Args[0] = translateUIExpression(value.Args[0], texts)
		}
		if name == "choice" && len(value.Args) == 3 {
			value.Args[1] = translateUIExpression(value.Args[1], texts)
			value.Args[2] = translateUIExpression(value.Args[2], texts)
		}
	}
	return expression
}

func loadCatalogs(dir string) map[string]catalog {
	result := make(map[string]catalog, 3)
	for _, language := range []string{"en", "zh-CN", "ja"} {
		path := filepath.Join(dir, language+".json")
		data, err := os.ReadFile(path)
		if err != nil {
			result[language] = catalog{}
			continue
		}
		var entries catalog
		if json.Unmarshal(data, &entries) != nil || entries == nil {
			fail(fmt.Errorf("invalid catalog %s", path))
		}
		result[language] = entries
	}
	return result
}

func writeCatalog(path string, entries catalog) error {
	data, err := json.MarshalIndent(entries, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(path, append(data, '\n'), 0o644)
}

func sortedKeys(values map[string]bool) []string {
	keys := make([]string, 0, len(values))
	for key := range values {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	return keys
}

func readRustCatalog(appPath, translatedPath string) (map[string]string, map[string]string) {
	english := make(map[string]string)
	file, err := os.Open(appPath)
	if err == nil {
		scanner := bufio.NewScanner(file)
		key := ""
		for scanner.Scan() {
			line := scanner.Text()
			if !strings.HasPrefix(line, "  ") && strings.HasSuffix(line, ":") {
				key = strings.TrimSuffix(line, ":")
				continue
			}
			trimmed := strings.TrimSpace(line)
			if key == "" || !strings.HasPrefix(trimmed, "en:") {
				continue
			}
			value := strings.TrimSpace(strings.TrimPrefix(trimmed, "en:"))
			english[key] = yamlScalar(value)
		}
		_ = file.Close()
	}
	translated := make(map[string]string)
	file, err = os.Open(translatedPath)
	if err != nil {
		return english, translated
	}
	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		line := scanner.Text()
		key, value, ok := strings.Cut(line, ":")
		if !ok || key == "_version" {
			continue
		}
		translated[strings.TrimSpace(key)] = yamlScalar(strings.TrimSpace(value))
	}
	_ = file.Close()
	return english, translated
}

func yamlScalar(value string) string {
	if strings.HasPrefix(value, "\"") {
		if decoded, err := strconv.Unquote(value); err == nil {
			return decoded
		}
	}
	if strings.HasPrefix(value, "'") && strings.HasSuffix(value, "'") && len(value) >= 2 {
		return strings.ReplaceAll(value[1:len(value)-1], "''", "'")
	}
	return value
}

func fail(err error) {
	fmt.Fprintln(os.Stderr, err)
	os.Exit(2)
}
