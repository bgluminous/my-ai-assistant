import { Marked } from "./vendor/marked.esm.js";

const markdown = new Marked({ gfm: true, breaks: true });
const rendered = new WeakMap();

export function releaseNoteUrl(value) {
  try {
    const url = new URL(value);
    return ["https:", "http:"].includes(url.protocol) && !url.username && !url.password
      ? url.href : null;
  } catch { return null; }
}

// 只读取 Markdown token 并创建固定的 DOM 元素，不把日志交给 innerHTML。
// 原始 HTML 和图片作为文字显示，避免脚本、事件属性及远程图片请求。
function fragmentFor(document, source) {
  const fragment = document.createDocumentFragment();
  const decoder = document.createElement("span");
  const decode = (text) => text.replace(/&(?:#\d+|#x[\da-f]+|[a-z][\da-z]*);/gi, (entity) => {
    // 只有单个字符实体进入解析器；解码结果仍只能写入文本或经校验的属性。
    decoder.innerHTML = entity;
    return decoder.textContent;
  });
  const element = (parent, tag) => parent.appendChild(document.createElement(tag));
  const text = (parent, value) => parent.appendChild(document.createTextNode(value));

  function append(parent, tokens, depth = 0, loose = false) {
    if (depth > 64) throw new Error("Release notes nesting limit");
    for (const token of tokens) {
      const children = (node) => append(node, token.tokens || [], depth + 1);
      switch (token.type) {
        case "space": case "def": break;
        case "heading": children(element(parent, `h${token.depth}`)); break;
        case "paragraph": children(element(parent, "p")); break;
        case "blockquote": children(element(parent, "blockquote")); break;
        case "strong": case "em": case "del": children(element(parent, token.type)); break;
        case "br": case "hr": element(parent, token.type); break;
        case "code": text(element(element(parent, "pre"), "code"), token.text); break;
        case "codespan": text(element(parent, "code"), token.text); break;
        case "html": case "escape": text(parent, token.text); break;
        case "image": text(parent, decode(token.text)); break;
        case "checkbox": text(parent, token.checked ? "☑ " : "☐ "); break;
        case "text":
          if (token.tokens) children(loose ? element(parent, "p") : parent);
          else text(parent, token.escaped ? token.text : decode(token.text));
          break;
        case "link": {
          const href = releaseNoteUrl(token.autolink ? token.href : decode(token.href));
          if (!href) { children(parent); break; }
          const link = element(parent, "a");
          link.setAttribute("href", href);
          link.setAttribute("target", "_blank");
          link.setAttribute("rel", "noopener noreferrer");
          if (token.title) link.setAttribute("title", decode(token.title));
          children(link);
          break;
        }
        case "list": {
          const list = element(parent, token.ordered ? "ol" : "ul");
          if (token.ordered) list.setAttribute("start", String(token.start));
          for (const item of token.items) {
            append(element(list, "li"), item.tokens, depth + 1, item.loose);
          }
          break;
        }
        case "table": {
          const table = element(parent, "table");
          const row = (section, cells, tag) => {
            const tr = element(section, "tr");
            for (const cell of cells) {
              const td = element(tr, tag);
              if (["left", "center", "right"].includes(cell.align)) td.style.textAlign = cell.align;
              append(td, cell.tokens, depth + 1);
            }
          };
          row(element(table, "thead"), token.header, "th");
          const body = element(table, "tbody");
          for (const cells of token.rows) row(body, cells, "td");
          break;
        }
        default: text(parent, token.raw || "");
      }
    }
  }
  append(fragment, markdown.lexer(source));
  return fragment;
}

export function renderReleaseNotes(container, source) {
  // 下载进度会频繁发出状态事件；内容不变时保留滚动位置、焦点与文本选择。
  if (rendered.get(container) === source) return;
  try {
    const fragment = fragmentFor(container.ownerDocument, source);
    container.replaceChildren(fragment);
    container.classList.remove("update-notes-plain");
  } catch {
    // 异常日志不能中断更新界面；安全退回可复制的原文。
    container.textContent = source;
    container.classList.add("update-notes-plain");
  }
  container.scrollTop = 0;
  rendered.set(container, source);
}
