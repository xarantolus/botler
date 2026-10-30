// @vitest-environment happy-dom
import { describe, expect, it } from "vitest";
import { detectLanguage, highlight } from "./highlight";
import { renderMarkdown } from "./markdown";

describe("detectLanguage", () => {
  it("recognises diffs", () => {
    const git = "diff --git a/x.py b/x.py\nindex 1..2 100644\n--- a/x.py\n+++ b/x.py\n@@ -1,2 +1,2 @@\n-a = 1\n+a = 2\n b = 3\n";
    expect(detectLanguage(git)).toBe("diff");
    expect(detectLanguage("@@ -3,1 +3,2 @@\n context\n-old\n+new\n+more\n")).toBe("diff");
    // A Markdown list is not a diff.
    expect(detectLanguage("- one\n- two\n+ three")).not.toBe("diff");
  });

  it("recognises JSON, shell, Python, TOML, YAML, Rust and TypeScript", () => {
    expect(detectLanguage('{"results": [{"title": "a", "url": "https://x"}]}')).toBe("json");
    expect(detectLanguage("[1, 2, 3]")).toBe("json");
    expect(detectLanguage("cargo test --workspace && git status")).toBe("bash");
    expect(detectLanguage("$ ls -la /tmp")).toBe("bash");
    expect(detectLanguage("def greet(name: str) -> str:\n    return f'hi {name}'\n\nimport os\nprint(greet(os.getcwd()))\n")).toBe("python");
    expect(detectLanguage('[package]\nname = "botler"\nversion = "0.1.0"\n\n[dependencies]\nserde = "1"\n')).toBe("ini");
    expect(detectLanguage("services:\n  web:\n    image: nginx\n    ports:\n      - \"80:80\"\n")).toBe("yaml");
    expect(detectLanguage("fn main() {\n    let mut v: Vec<u32> = Vec::new();\n    v.push(1);\n    println!(\"{:?}\", v);\n}\n")).toBe("rust");
    expect(detectLanguage("interface User { name: string; age?: number }\nexport const f = (u: User): string => u.name;\n")).toBe("typescript");
  });

  it("leaves prose and broken JSON plain", () => {
    expect(detectLanguage("The weather in Augsburg is mild today, with some rain expected in the evening.")).toBeNull();
    expect(detectLanguage("Done. I updated the file and pushed the branch.")).toBeNull();
    expect(detectLanguage("Summary: the build passed.\nNext: deploy it to the server tomorrow.")).toBeNull();
    expect(detectLanguage("I looked into it. The function returns None if the input is empty, so the caller must check.")).toBeNull();
    expect(detectLanguage('{"a": 1,')).not.toBe("json");
  });
});

describe("highlight", () => {
  it("uses a given language and escapes the code", () => {
    const { html, language } = highlight('<script>alert("x")</script>', "python");
    expect(language).toBe("python");
    expect(html).not.toContain("<script>");
    expect(highlight("x = 1", "toml").language).toBe("toml");
  });

  it("colors added and removed lines of a diff", () => {
    const { html } = highlight("@@ -1 +1 @@\n-old\n+new\n");
    expect(html).toContain('class="hljs-deletion"');
    expect(html).toContain('class="hljs-addition"');
  });

  it("highlights Markdown fences, with or without a language", () => {
    expect(renderMarkdown("```rust\nfn main() {}\n```")).toContain('data-language="rust"');
    expect(renderMarkdown('```\n{"a": [1, 2]}\n```')).toContain('data-language="json"');
    expect(renderMarkdown("```\n<img src=x onerror=alert(1)>\n```")).not.toContain("<img");
  });
});
