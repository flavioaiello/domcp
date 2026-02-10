class Domcp < Formula
  desc "Domain Model Context Protocol Server — architectural meta-layer for GitHub Copilot"
  homepage "https://github.com/flavioaiello/domcp.ai"
  license "MIT"
  version "0.1.0"

  # Update URL and sha256 when publishing a release
  # url "https://github.com/flavioaiello/domcp.ai/archive/refs/tags/v#{version}.tar.gz"
  # sha256 "REPLACE_WITH_ACTUAL_SHA256"

  # For local development / testing:
  head "https://github.com/flavioaiello/domcp.ai.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
    # Binary is named domcp
  end

  def post_install
    # Ensure the data directory exists
    (var/"domcp").mkpath
  end

  def caveats
    <<~EOS
      DOMCP stores domain models in ~/.domcp/domcp.db (SQLite).

      To use with VS Code / GitHub Copilot, add to .vscode/mcp.json:

        {
          "servers": {
            "domcp": {
              "type": "stdio",
              "command": "domcp",
              "args": ["serve", "--workspace", "${workspaceFolder}"]
            }
          }
        }

      To import an existing domcp.json:

        domcp import domcp.json --workspace /path/to/your/project

      To list all stored projects:

        domcp list
    EOS
  end

  test do
    # Verify the binary starts and prints usage
    output = shell_output("#{bin}/domcp 2>&1", 1)
    assert_match "domcp", output
  end
end
