```
codex --disable shell_tool --config 'mcp_servers.agfs={command = "/users/szhong/agentctl/target/debug/agfs", args = ["--mcp"]}' 'Use the shell tool to run this exact command: printf "hello from agfs\n" > smoke.txt. check the result, but instead of commiting, abort the changes.'
```

## Build & Install

Build everything (CLI + kernel module):
```bash
make
```

Build individually:
```bash
make cli    # build the CLI (Rust, release mode)
make kmod   # build the kernel module
```

Install CLI and load kernel module:
```bash
sudo make install
```

Uninstall CLI and unload kernel module:
```bash
sudo make uninstall
```

Clean build artifacts:
```bash
make clean
```

## Dependencies

Install uv
```bash
curl -LsSf https://astral.sh/uv/install.sh | sh
```

Install npm v24.13.1
```bash
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.3/install.sh | bash
bash -c ". ~/.nvm/nvm.sh && nvm install 24.13.1"
if set -q FISH_VERSION; fish_add_path ~/.nvm/versions/node/v24.13.1/bin; end
```

Install Claude 2.1.45
```bash
curl -fsSL https://claude.ai/install.sh | bash -s -- 2.1.45
```

Install Codex 0.101.0
```bash
npm i -g @openai/codex@0.101.0
```

Install OpenCode 1.2.6
```bash
curl -fsSL https://opencode.ai/install | bash -s -- --version 1.2.6
if set -q FISH_VERSION; fish_add_path ~/.opencode/bin; end
```

Install Gemini CLI 0.29.0
```bash
npm install -g @google/gemini-cli@0.29.0
```

Install GitHub Copilot CLI 0.0.411
```bash
curl -fsSL https://gh.io/copilot-install | VERSION=0.0.411 bash
```

