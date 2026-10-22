import changelog from "../../../CHANGELOG.md?raw";
import { Panel } from "../design/index.js";

// Our curated changelog uses headings, paragraphs and bullet lists. Render text as React nodes,
// never HTML: issue references become links (when the build sets VITE_ISSUE_URL, e.g. in
// web/.env.local), and a future raw HTML line stays harmless text.
const ISSUE_URL = import.meta.env.VITE_ISSUE_URL ?? "";

function issueLinks(text: string) {
  if (!ISSUE_URL) return text;
  return text.split(/(H15-\d+)/).map((part, i) => /^H15-\d+$/.test(part)
    ? <a key={i} href={ISSUE_URL.replace("{id}", part)}>{part}</a>
    : part);
}

export default function Changelog() {
  const blocks = changelog.trim().split(/\n\s*\n/);
  return <Panel title="What’s new" className="changelog">
    {blocks.map((block, i) => {
      if (block.startsWith("# ")) return <p key={i}>{block.slice(2)}</p>;
      if (block.startsWith("## ")) return <h2 key={i}>{block.slice(3)}</h2>;
      if (block.startsWith("- ")) return <ul key={i}>{block.split("\n").map((line, j) =>
        <li key={j}>{issueLinks(line.replace(/^- /, ""))}</li>)}</ul>;
      return <p key={i}>{issueLinks(block)}</p>;
    })}
  </Panel>;
}
