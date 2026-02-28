# devo

devo は、シンプルな TOML DSL から tmux セッション用のシェルコマンドを生成し、`bash` 経由で実行する CLI ツールです。

## quick start

```bash
nix develop
make build
make plan
```

実行:

```bash
make run
```

## dsl

設定ファイルは `devo.toml` です。主なキー:

- `session`: tmux セッション名
- `tmux_bin`: tmux コマンドパス（省略時 `tmux`）
- `hook_session_closed`: `session-closed` フック
- `[env]`: 実行前に export する環境変数
- `[[tasks]]`: タスク定義
  - `id`: タスクID
  - `pane`: `root` / `right_of:<task-id>` / `down_of:<task-id>`
  - `cmd`: そのペインで実行するコマンド（複数行可）
  - `depends_on`: 依存タスクID配列
- `focus`: 最後にフォーカスするタスクID

## commands

```bash
cargo run -- plan -f devo.toml
cargo run -- run -f devo.toml
```

`plan` は生成シェルを出力し、`run` は生成シェルを `bash` で実行します。
