# AGENTS.md

编码与协作规则见子模块 [coding-rule/README.md](coding-rule/README.md)。按其中索引读取通用规则与本项目文档，不要在本文件复制维护。

若 `coding-rule/README.md` 不存在或不可读，先在仓库根目录执行 `git submodule update --init --recursive`（不要加 `--remote`），再读取规则。
每次修改前必须 `git -C coding-rule fetch origin`；若有新提交，先 `git submodule update --remote coding-rule` 再改动。子模块工作区不干净时必须停止并询问，不得强行更新。
