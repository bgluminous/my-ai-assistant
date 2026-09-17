# AGENTS.md

编码与协作规则见 [coding-rule/README.md](coding-rule/README.md)。
按其中索引读取通用规则与本项目文档，不要在本文件复制维护。

`coding-rule/` 是不入库的独立克隆（已在 `.gitignore` 中忽略），其仓库地址不得写入本仓库任何文件。
若 `coding-rule/README.md` 不存在或不可读，向用户索取仓库地址并克隆到 `coding-rule/`，再读取规则。
每次修改前必须 `git -C coding-rule fetch origin`；若有新提交，先 `git -C coding-rule pull --ff-only`
再改动。`coding-rule/` 工作区不干净时必须停止并询问，不得强行更新。
