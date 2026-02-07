# VHS demo tape for yoetz
# Install: https://github.com/charmbracelet/vhs
# Run: vhs demo.tape

Output demo.gif
Set FontSize 16
Set Width 1200
Set Height 600
Set Theme "Catppuccin Mocha"
Set Padding 20

Type "# yoetz - Fast LLM council for coding agents"
Enter
Sleep 1s

Type "# Ask a single model"
Enter
Type "yoetz ask -p 'Explain the difference between async and sync Rust' --provider openai --model gpt-5.2"
Enter
Sleep 4s

Type "# Multi-model council - get consensus from 3 providers"
Enter
Type "yoetz council -p 'Should we use channels or shared state?' --models openai/gpt-5.2,gemini/gemini-pro-3,openrouter/xai/grok-4.1 --format json"
Enter
Sleep 5s

Type "# Bundle code files for LLM context"
Enter
Type "yoetz bundle -p 'Review this codebase' -f 'crates/**/*.rs' --format json"
Enter
Sleep 3s

Type "# Code review on staged changes"
Enter
Type "yoetz review diff --staged --format json"
Enter
Sleep 3s

Type "# Estimate costs before running"
Enter
Type "yoetz pricing estimate --model gpt-5.2 --input-tokens 10000 --output-tokens 1000"
Enter
Sleep 3s
