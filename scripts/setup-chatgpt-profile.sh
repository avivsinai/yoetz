#!/bin/bash
# Setup script for ChatGPT browser profile
# This opens a headed browser for you to log in to ChatGPT
# The session will be saved for future headless use

PROFILE_DIR="${AGENT_BROWSER_PROFILE:-$HOME/.chatgpt-profile}"

echo "Opening ChatGPT in headed browser..."
echo "Profile will be saved to: $PROFILE_DIR"
echo ""
echo "Instructions:"
echo "1. Log in to your ChatGPT account"
echo "2. Close the browser when done"
echo ""

agent-browser --profile "$PROFILE_DIR" --headed open "https://chatgpt.com/"

echo ""
echo "Profile saved. You can now use:"
echo "  export AGENT_BROWSER_PROFILE=$PROFILE_DIR"
echo "  yoetz browser recipe --recipe recipes/chatgpt.yaml --bundle your-prompt.txt"
