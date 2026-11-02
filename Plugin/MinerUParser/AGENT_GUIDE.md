# MinerUParser：解析、论文库与渐进式阅读

你可以将本指南作为 MinerU 工具提示词。以实际工具回执为准；不要猜测参数、文件位置或解析状态。

## 工作顺序与硬边界

1. **先查库，再解析**：已有论文先 `list` 搜索，再 `info` 定位。导入本地 PDF 前用 `preflight` 查重；已解析不重复上传，未完成先检查是否可 `query` 续查。
2. **正文按需读**：`parse/batch/query` 返回元数据卡片，不自动灌入全文。用 `read` 按章节、行范围或关键词读取，再选图核对。
3. **区分观察与执行**：`status` 只查看本地记录，不访问云端，不证明进程仍活着；`query` 才会联网续查、下载或收尾。不得未经要求反复轮询、重新上传。
4. **不能把裁剪当完整**：`list/info` 是有展示上限的卡片；`limit` 是数量上限，不是分页。需要完整机器元数据用 `library_view/paper_view`。正文即使 `lines=all` 仍有长行裁切及资源上限。
5. **默认只读预演**：`migrate/reindex/link_pdf/unlink_pdf` 默认预演。写入须明确需要、核对预演结果，再按各命令契约提交；不要把一次写入失败当成完全未落盘。
6. **不混淆来源**：原解析来源和本机阅读 PDF 绑定独立；禁止为了打开 PDF 直接改 `source_path`、伪造哈希或放宽身份检查。
7. **未知不编造**：未提供的图注、页码、统计写未知；图片文件数不等于论文 Figure 数。不得仅据元数据声称看过图或读过正文。

## 命令速查

以下是参数速查，不是函数调用语法。实际 VCP 调用使用 tool_name=MinerUParser、command=命令名，各参数独立成字段。

| 命令 | 作用 | 主要参数与边界 |
|---|---|---|
| list | 本地库搜索与清单 | query?、status?、sort?、limit?；默认20，最多100；无分页游标 |
| status | 本地进度记录 | paper_id?/paper_ids?、status?、limit?；默认50，最多200；不耗云解析配额 |
| info | 单篇/多篇阅读定位卡片 | paper_id?/paper_ids?；支持可唯一解析的标题关键词，卡片数组可能裁剪 |
| preflight | 导入前查重 | file_paths/paths 或 urls；recursive?；本地PDF最多500个；不提交解析 |
| duplicates | 全库重复线索 | group_by=all/asset/text/title/source、query?、limit?；只读，不自动删除 |
| parse | 单PDF/公开URL解析 | file_path 或 url；title?、model_version?、language?、is_ocr?、enable_formula?、enable_table? |
| batch | 多PDF/目录/URL解析 | file_paths/urls、recursive?、workers?；默认3，范围1–5；单次最多20文件 |
| query | 已提交任务续查 | paper_id/paper_ids、batch_id/batch_ids、all?、workers?；最多10篇一批；all是选择范围，不承诺一次全部完成 |
| locate | 内容块关键词定位 | paper_id、keyword；返回页码、块类型与摘录，不等于已读取全部相关正文 |
| read | 正文与图片切片 | paper_id，加下节选择器；默认文件full.md |
| library_view | 完整机器库元数据 | 无选择参数；按paper_id去重，不含正文、完整大纲、图片清单或PDF绑定 |
| paper_view | 单篇完整机器元数据 | 精确paper_id；完整index/state，不含正文/PDF字节 |
| migrate | 整理旧资产目录名 | paper_id/paper_ids/all?、apply?；默认预演，保持paper_id，不是图片重索引 |
| reindex | 本地图片清单与统计对账 | paper_id/paper_ids或显式all=true预演；单篇受控提交见下文 |
| get_pdf_binding | 查询阅读PDF绑定 | paper_id；不核验PDF当前存在或仍匹配 |
| link_pdf | 核验/绑定本机阅读PDF | paper_id、file_path；默认预演；提交需双修订条件 |
| unlink_pdf | 禁用阅读绑定 | paper_id；默认预演；提交需expected_revision；不删除PDF |

标题关键词必须唯一命中；歧义时先list拿精确paper_id。机器详情paper_view必须精确ID，不以标题代替。

## read：先定位，再补读

正文选择：
- `lines=head:50 / tail:50 / 100-160 / 100:160 / 120 / all`，行号从1开始。
- `section=3.2 Experimental Setup` 或章节关键词，包含子节。
- `find=microwave absorption`，`context=2`（最多10）返回命中与上下文。
- `path=full.md`、`paths`（数组或逗号/换行分隔）、`glob=**/*.md`；单路径可带`:100-160`。
- `show_line=false` 可关闭行号。

图片选择：
- `images=1,2`：索引序号从1开始，不等于论文Figure编号。
- `images=page:4` 或 `page=4`：展示页码从1开始。
- `images=caption:Figure 2` 或 `caption=Figure 2`：图注关键词。
- 已知资产路径也可 `path=images/xxx.jpg`。
- `with_base64=true` 默认将图片作为image_url交给视觉通道；只有实际取得图像后才能声称读图。
- 单次最多20个文本文件、12张图片；单图10MB、图片合计24MB。超限分批，不通过其它文件工具绕过论文资产路径边界。

**长行与裁切：**
- 缺省行选择最多600行；长行每段最多1000个Unicode字符。
- 检查回执的裁切、命中上限及 `files[].clipped_lines[]`，不把片段称作全文。
- 取 `next_cursor` 原样续读：只传相同paper_id、cursor及可选show_line，不混入lines/section/find/path等选择器。
- 续读直到该行的 `files[].next_cursor=null`；这只表示该行结束，不代表所有文件已读完。
- `STALE_CURSOR` 表示文件内容变化，重新定位，不修改游标。
- find居中摘录的游标会从行首补读，不直接与原摘录拼接，以免重复或错位。
- 不猜测未知元数据字段；若回执通道只显示卡片而未显示data，不能声称已核验结构化完整性。

## 完整元数据不是无限读取

`library_view` 返回schema=mineru.library-view；`paper_view` 返回schema=mineru.paper-view，均为schema_version=1。核对complete=true、truncated=false、身份和revision。
- 库元数据超过4MB、超过5000目录项或遇到坏JSON/身份不匹配等会明确失败，不将部分结果当全库。
- 单篇详情总元数据上限4MB；缺失index/state可为null。
- best_effort_scan不是跨文件事务快照；revision不是全库写事务授权。
- info卡片中的图片数、磁盘图片数、内联引用数、内容图块数分口径理解；未知统计保留null。

## 受控写操作

### 重索引
1. `command=reindex, paper_id=精确ID` 预演；全库仅预演可 `all=true`。
2. 核对逐项audit、inventory_preview与index_revision。
3. 明确提交时，仅单个paper_id，传 `apply=true, expected_revision=该index_revision`。
4. 保留旧图片顺序与扩展字段，只更新图片清单、统计及来源；不改state、正文或图片文件，不调用云端。
5. 返回备份与新修订；旧修订重试冲突时查询现状，不盲目重复写。存在重复资产目录歧义先处理，不猜规范目录。

### PDF阅读绑定
1. `link_pdf` 使用精确paper_id与绝对file_path，PDF不超过200MB，先预演。
2. 仅can_apply=true才自动提交；传 `apply=true, expected_revision=返回的binding_revision, expected_pdf_sha256=返回的sha256`。首次修订可能为missing。
3. 文件hash身份不匹配、URL来源无法用PDF内容认证时拒绝；不自创人工确认参数。
4. `unlink_pdf` 提交传apply=true和预演binding_revision；写disabled标记防旧映射回退，不删除PDF。
5. 本机路径不可假定跨设备可用；get_pdf_binding成功不证明PDF仍存在或哈希仍一致。

### 目录整理
`migrate` 默认预演，确认后apply=true；不要与reindex或旧PDF映射迁移混为一谈。不随意批量改真实资产目录。

## 调用字段示例（按标准VCP格式包装）

- 查论文：`command=list`，`query=microwave absorption`，`limit=10`。
- 看未完成记录：`command=status`，`status=running,timeout`。
- 本地导入预检：`command=preflight`，`file_paths=D:\papers`，`recursive=true`。
- 批量解析：`command=batch`，`file_paths=D:\papers\a.pdf,D:\papers\b.pdf`，`workers=3`。
- 显式续查：`command=query`，`paper_ids=paper_xxx,paper_yyy`，`workers=3`。
- 按章节阅读：`command=read`，`paper_id=paper_xxx`，`section=3.2`。
- 查关键词：`command=locate`，`paper_id=paper_xxx`，`keyword=scaled dot-product`。
- 读图：`command=read`，`paper_id=paper_xxx`，`images=1,2`。
- 精确完整详情：`command=paper_view`，`paper_id=paper_xxx`。
- 长行续读：`command=read`，`paper_id=paper_xxx`，`cursor=回执原样游标`。

命令名是纯字符串，例如migrate；不要加入多余空格或错误界定符。未列出的参数先查manifest，不按自然语言自行拼新参数。