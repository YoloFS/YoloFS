Install uv
```bash
curl -LsSf https://astral.sh/uv/install.sh | sh
```

Install npm v24.13.1
```bash
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.3/install.sh | bash
bash -c ". ~/.nvm/nvm.sh && nvm install 24.13.1"
# for fish: fish_add_path ~/.nvm/versions/node/v24.13.1/bin
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
# for fish: fish_add_path ~/.opencode/bin
```

Install Gemini CLI 0.29.0
```bash
npm install -g @google/gemini-cli@0.29.0
```

Install GitHub Copilot CLI 0.0.411
```bash
curl -fsSL https://gh.io/copilot-install | VERSION=0.0.411 bash
```

