// The Turnframe design system components (React), ported from its bundle to an ES module.
// Data the site owns (scenarios, measured figures, crates) is passed in from src/data.
import React from "react";
import { SCENARIOS } from "../data/scenarios.js";

var
  { useState, useEffect, useRef, useCallback } = React,
  cx = (...a) => a.filter(Boolean).join(" ");
function useReducedMotion() {
  let [r, setR] = useState(!1);
  return (
    useEffect(() => {
      let m = window.matchMedia ? window.matchMedia("(prefers-reduced-motion: reduce)") : null;
      if (!m) return;
      setR(m.matches);
      let f = (e) => setR(e.matches);
      return (
        m.addEventListener ? m.addEventListener("change", f) : m.addListener(f),
        () => (m.removeEventListener ? m.removeEventListener("change", f) : m.removeListener(f))
      );
    }, []),
    r
  );
}
var P = {
  copy: React.createElement(
    React.Fragment,
    null,
    React.createElement("rect", { x: "5.5", y: "5.5", width: "8", height: "8", rx: "1.5" }),
    React.createElement("path", { d: "M10.5 3.5v-.5a1 1 0 0 0-1-1H3.5a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h.5" }),
  ),
  check: React.createElement("path", { d: "M3 8.5l3 3 7-7" }),
  x: React.createElement("path", { d: "M4 4l8 8M12 4l-8 8" }),
  play: React.createElement("path", {
    d: "M5 3.2v9.6a.5.5 0 0 0 .77.42l7.2-4.8a.5.5 0 0 0 0-.84l-7.2-4.8A.5.5 0 0 0 5 3.2z",
    fill: "currentColor",
    stroke: "none",
  }),
  pause: React.createElement(
    React.Fragment,
    null,
    React.createElement("rect", {
      x: "4",
      y: "3",
      width: "2.6",
      height: "10",
      rx: ".6",
      fill: "currentColor",
      stroke: "none",
    }),
    React.createElement("rect", {
      x: "9.4",
      y: "3",
      width: "2.6",
      height: "10",
      rx: ".6",
      fill: "currentColor",
      stroke: "none",
    }),
  ),
  step: React.createElement(
    React.Fragment,
    null,
    React.createElement("path", {
      d: "M3.5 3.6v8.8a.4.4 0 0 0 .62.33l6.3-4.4a.4.4 0 0 0 0-.66l-6.3-4.4a.4.4 0 0 0-.62.33z",
      fill: "currentColor",
      stroke: "none",
    }),
    React.createElement("path", { d: "M12.5 3v10" }),
  ),
  restart: React.createElement(
    React.Fragment,
    null,
    React.createElement("path", { d: "M2.8 8a5.2 5.2 0 1 0 1.6-3.75" }),
    React.createElement("path", { d: "M2.5 2.5v3h3" }),
  ),
  arrow: React.createElement("path", { d: "M3 8h10M9 4l4 4-4 4" }),
  external: React.createElement(
    React.Fragment,
    null,
    React.createElement("path", { d: "M9.5 2.5h4v4" }),
    React.createElement("path", { d: "M13.5 2.5L7.5 8.5" }),
    React.createElement("path", { d: "M11.5 9.5v3a1 1 0 0 1-1 1h-7a1 1 0 0 1-1-1v-7a1 1 0 0 1 1-1h3" }),
  ),
  sun: React.createElement(
    React.Fragment,
    null,
    React.createElement("circle", { cx: "8", cy: "8", r: "2.8" }),
    React.createElement("path", {
      d: "M8 1.5v1.3M8 13.2v1.3M1.5 8h1.3M13.2 8h1.3M3.4 3.4l.9.9M11.7 11.7l.9.9M3.4 12.6l.9-.9M11.7 4.3l.9-.9",
    }),
  ),
  moon: React.createElement("path", { d: "M13.2 9.6A5.6 5.6 0 0 1 6.4 2.8a5.6 5.6 0 1 0 6.8 6.8z" }),
  system: React.createElement(
    React.Fragment,
    null,
    React.createElement("rect", { x: "1.8", y: "2.8", width: "12.4", height: "8.4", rx: "1.2" }),
    React.createElement("path", { d: "M5.5 13.8h5M8 11.2v2.6" }),
  ),
  menu: React.createElement("path", { d: "M2.5 4.5h11M2.5 8h11M2.5 11.5h11" }),
  receipt: React.createElement(
    React.Fragment,
    null,
    React.createElement("path", { d: "M3.5 1.8h9v12.4l-1.5-1-1.5 1-1.5-1-1.5 1-1.5-1-1.5 1z" }),
    React.createElement("path", { d: "M6 7.8l1.5 1.5 2.8-2.8" }),
  ),
  notice: React.createElement(
    React.Fragment,
    null,
    React.createElement("path", { d: "M8 1.8l6.5 11.4h-13z" }),
    React.createElement("path", { d: "M8 6.5v3M8 11.3v.2" }),
  ),
  card: React.createElement(
    React.Fragment,
    null,
    React.createElement("rect", { x: "1.8", y: "3", width: "12.4", height: "10", rx: "1.5" }),
    React.createElement("path", { d: "M4.5 6.5h7M4.5 9.5h4" }),
  ),
  bolt: React.createElement("path", { d: "M9 1.5L3.5 9h4l-1 5.5L12.5 7h-4z" }),
  split: React.createElement(
    React.Fragment,
    null,
    React.createElement("rect", { x: "1.8", y: "2.8", width: "12.4", height: "10.4", rx: "1.2" }),
    React.createElement("path", { d: "M8 2.8v10.4" }),
  ),
  user: React.createElement(
    React.Fragment,
    null,
    React.createElement("circle", { cx: "8", cy: "5.5", r: "2.7" }),
    React.createElement("path", { d: "M2.8 14c.6-2.6 2.7-4.2 5.2-4.2s4.6 1.6 5.2 4.2" }),
  ),
  book: React.createElement(
    React.Fragment,
    null,
    React.createElement("path", {
      d: "M2.5 3a1 1 0 0 1 1-1h3.5A1.5 1.5 0 0 1 8.5 3.5V14a1.5 1.5 0 0 0-1.5-1.5H3.5a1 1 0 0 1-1-1z",
    }),
    React.createElement("path", {
      d: "M13.5 3a1 1 0 0 0-1-1H9A1.5 1.5 0 0 0 7.5 3.5V14A1.5 1.5 0 0 1 9 12.5h3.5a1 1 0 0 0 1-1z",
    }),
  ),
  up: React.createElement("path", { d: "M4 10l4-4 4 4" }),
  down: React.createElement("path", { d: "M4 6l4 4 4-4" }),
  lock: React.createElement(
    React.Fragment,
    null,
    React.createElement("rect", { x: "3.5", y: "7", width: "9", height: "7", rx: "1" }),
    React.createElement("path", { d: "M5.5 7V5a2.5 2.5 0 0 1 5 0v2" }),
  ),
  terminal: React.createElement(
    React.Fragment,
    null,
    React.createElement("rect", { x: "1.8", y: "2.8", width: "12.4", height: "10.4", rx: "1.2" }),
    React.createElement("path", { d: "M4.5 6l2 2-2 2M8.5 10.5h3" }),
  ),
};
function Icon({ name, size = 16, label, className }) {
  return React.createElement(
    "svg",
    {
      className: cx("tf-icon", className),
      width: size,
      height: size,
      viewBox: "0 0 16 16",
      fill: "none",
      stroke: "currentColor",
      strokeWidth: "1.5",
      strokeLinecap: "round",
      strokeLinejoin: "round",
      "aria-hidden": label ? void 0 : "true",
      role: label ? "img" : void 0,
      "aria-label": label,
    },
    P[name] || null,
  );
}
Icon.names = Object.keys(P);
var MARK = React.createElement(
  React.Fragment,
  null,
  React.createElement("path", { className: "br", d: "M9 3H4v26h5M23 3h5v26h-5" }),
  React.createElement("path", { className: "kf", d: "M16 9.5L22.5 16L16 22.5L9.5 16Z" }),
);
function Logo({ variant = "lockup", size = 24, animate, className }) {
  let mark = React.createElement(
      "svg",
      {
        className: "tf-logo__mark",
        width: size,
        height: size,
        viewBox: "0 0 32 32",
        "aria-hidden": "true",
      },
      MARK,
    ),
    cls = cx("tf-logo", animate && "tf-logo--play", className);
  return variant === "mark"
    ? React.createElement("span", { className: cls, role: "img", "aria-label": "Turnframe" }, mark)
    : React.createElement(
        "span",
        {
          className: cls,
          style: { fontSize: size * 0.8, gap: size * 0.34 },
          role: "img",
          "aria-label": "Turnframe",
        },
        mark,
        React.createElement("span", { className: "tf-logo__word", "aria-hidden": "true" }, "turnframe"),
      );
}
function Button({
  variant = "secondary",
  size = "md",
  href,
  icon,
  iconRight,
  children,
  className,
  label,
  ...rest
}) {
  let cls = cx(
      "tf-btn",
      `tf-btn--${variant}`,
      size !== "md" && `tf-btn--${size}`,
      !children && "tf-btn--icon",
      className,
    ),
    inner = React.createElement(
      React.Fragment,
      null,
      icon && React.createElement(Icon, { name: icon }),
      children,
      iconRight && React.createElement(Icon, { name: iconRight }),
    );
  return href
    ? React.createElement("a", { className: cls, href, "aria-label": label, ...rest }, inner)
    : React.createElement(
        "button",
        { type: "button", className: cls, "aria-label": label, ...rest },
        inner,
      );
}
var TAG_TEXT = {
    proposed: "Proposed",
    decided: "Decided",
    committed: "Committed",
    held: "Held",
    refused: "Refused",
    notice: "Notice",
    awaiting: "Your move",
    neutral: "",
  },
  TAG_GLYPH = {
    proposed: "\u25CC",
    decided: "\u25A0",
    committed: "\u2713",
    held: "!",
    notice: "!",
    refused: "\u2715",
    awaiting: "\u2192",
    neutral: "\xB7",
  };
function StateTag({ state = "neutral", children, className }) {
  return React.createElement(
    "span",
    { className: cx("tf-tag", `tf-tag--${state}`, className) },
    React.createElement("span", { className: "tf-tag__g", "aria-hidden": "true" }, TAG_GLYPH[state]),
    children || TAG_TEXT[state],
  );
}
function Confirm({ children = "Confirmed", detail, tone = "lime", animate, className }) {
  return React.createElement(
    "span",
    {
      className: cx(
        "tf-confirm",
        tone !== "lime" && `tf-confirm--${tone}`,
        animate && "tf-confirm--in",
        className,
      ),
    },
    children,
    detail && React.createElement("small", null, detail),
  );
}
function Claim({ rev, children, draw, className }) {
  return React.createElement(
    "span",
    { className: cx("tf-claim", className) },
    React.createElement("span", { className: cx("tf-hl", draw && "tf-hl--draw") }, children),
    rev != null && React.createElement("sup", null, "rev ", rev),
  );
}
function withClaims(text, draw) {
  let out = [],
    re = /\[([^\]|]+)\|(\d+)\]/g,
    last = 0,
    m;
  for (; (m = re.exec(text));)
    (m.index > last && out.push(text.slice(last, m.index)),
      out.push(React.createElement(Claim, { key: m.index, rev: m[2], draw }, m[1])),
      (last = m.index + m[0].length));
  return (last < text.length && out.push(text.slice(last)), out);
}
function copyText(text) {
  try {
    if (navigator.clipboard && window.isSecureContext) return navigator.clipboard.writeText(text);
  } catch {}
  return new Promise((res) => {
    let t = document.createElement("textarea");
    ((t.value = text),
      (t.style.position = "fixed"),
      (t.style.opacity = "0"),
      document.body.appendChild(t),
      t.select());
    try {
      document.execCommand("copy");
    } catch {}
    (document.body.removeChild(t), res());
  });
}
function useCopied() {
  let [done, setDone] = useState(!1),
    timer = useRef(0);
  useEffect(() => () => clearTimeout(timer.current), []);
  let copy = useCallback((text) => {
    Promise.resolve(copyText(text)).then(() => {
      (setDone(!0), clearTimeout(timer.current), (timer.current = setTimeout(() => setDone(!1), 1600)));
    });
  }, []);
  return [done, copy];
}
function InstallCommand({ command = "cargo add turnframe", prompt = "$", className }) {
  let [done, copy] = useCopied();
  return React.createElement(
    "div",
    { className: cx("tf-install", className) },
    React.createElement("span", { className: "tf-install__prompt", "aria-hidden": "true" }, prompt),
    React.createElement("code", { className: "tf-install__cmd" }, command),
    React.createElement(Button, {
      variant: "ghost",
      size: "sm",
      icon: done ? "check" : "copy",
      label: done ? "Copied" : "Copy command",
      onClick: () => copy(command),
    }),
    React.createElement("span", { className: "tf-sr", "aria-live": "polite" }, done ? "Copied" : ""),
  );
}
function Segmented({ options, value, onChange, label, className }) {
  return React.createElement(
    "div",
    { className: cx("tf-seg", className), role: "radiogroup", "aria-label": label },
    options.map((o) => {
      let id = typeof o == "string" ? o : o.value,
        text = typeof o == "string" ? o : o.label;
      return React.createElement(
        "button",
        {
          key: id,
          type: "button",
          role: "radio",
          "aria-checked": value === id,
          className: "tf-seg__opt",
          title: o.title,
          onClick: () => onChange && onChange(id),
        },
        o.icon && React.createElement(Icon, { name: o.icon, size: 13 }),
        text,
      );
    }),
  );
}
function Frame({ children, label, className }) {
  return React.createElement(
    "div",
    { className: cx("tf-frame", className) },
    label &&
      React.createElement(
        "span",
        { className: "tf-frame__label" },
        React.createElement("b", null, "Rec"),
        label,
      ),
    React.createElement("span", { className: "tf-frame__c tf-frame__c--tl" }),
    React.createElement("span", { className: "tf-frame__c tf-frame__c--tr" }),
    React.createElement("span", { className: "tf-frame__c tf-frame__c--bl" }),
    React.createElement("span", { className: "tf-frame__c tf-frame__c--br" }),
    children,
  );
}
var React2 = React,
  { useState: useState2 } = React2,
  RUST_KW = new Set(
    "as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait true type unsafe use where while".split(
      " ",
    ),
  ),
  RULES = {
    rust: [
      ["co", /\/\/[^\n]*/y],
      ["st", /b?"(?:\\.|[^"\\])*"/y],
      ["ma", /#!?\[[^\]\n]*\]/y],
      ["ma", /[a-z_][A-Za-z0-9_]*!/y],
      ["nu", /'[a-z_]+\b(?!')/y],
      ["nu", /\b\d[\d_]*(?:\.\d+)?\b/y],
      ["id", /[A-Za-z_][A-Za-z0-9_]*/y],
      ["pu", /::|->|=>|[{}()[\];,.<>&?|=+\-*/:]/y],
    ],
    toml: [
      ["co", /#[^\n]*/y],
      ["st", /"(?:\\.|[^"\\])*"/y],
      ["ty", /^\s*\[[^\]\n]+\]/my],
      ["kw", /^[ \t]*[A-Za-z0-9_.-]+(?=\s*=)/my],
      ["nu", /\b\d[\d.]*\b/y],
      ["pu", /[{}[\],=]/y],
    ],
    sh: [
      ["co", /#[^\n]*/y],
      ["st", /"(?:\\.|[^"\\])*"/y],
      ["kw", /^\s*(?:cargo|cp|export|git)\b/my],
      ["nu", /--?[a-z-]+/y],
    ],
  };
function highlight(code, lang = "rust") {
  let rules = RULES[lang];
  if (!rules) return [code];
  let out = [],
    i = 0,
    plain = "",
    flush = () => {
      plain && (out.push(plain), (plain = ""));
    };
  for (; i < code.length;) {
    let hit = null;
    for (let [k2, re] of rules) {
      re.lastIndex = i;
      let m = re.exec(code);
      if (m && m.index === i && m[0].length) {
        hit = [k2, m[0]];
        break;
      }
    }
    if (!hit) {
      ((plain += code[i]), (i += 1));
      continue;
    }
    let [k, s] = hit;
    if (k === "id") {
      let after = code.slice(i + s.length);
      k = RUST_KW.has(s) ? "kw" : /^[A-Z]/.test(s) ? "ty" : /^\s*(\(|::<)/.test(after) ? "fn" : null;
    }
    (flush(),
      out.push(k ? React2.createElement("span", { key: i, className: `tk-${k}` }, s) : s),
      (i += s.length));
  }
  return (flush(), out);
}
function lines(nodes) {
  let rows = [[]];
  return (
    nodes.forEach((n, idx) => {
      typeof n == "string"
        ? n
            .split(
              `
`,
            )
            .forEach((part, j) => {
              (j && rows.push([]), part && rows[rows.length - 1].push(part));
            })
        : typeof n.props.children == "string" &&
            n.props.children.includes(`
`)
          ? n.props.children
              .split(
                `
`,
              )
              .forEach((part, j) => {
                (j && rows.push([]),
                  part &&
                    rows[rows.length - 1].push(
                      React2.createElement(
                        "span",
                        { key: `${idx}-${j}`, className: n.props.className },
                        part,
                      ),
                    ));
              })
          : rows[rows.length - 1].push(n);
    }),
    rows
  );
}
function CodeBlock({ tabs, code, lang = "rust", filename, numbered = !0, caption, maxHeight, className }) {
  let list = tabs || [{ name: filename || lang, lang, code }],
    [at, setAt] = useState2(0),
    cur = list[Math.min(at, list.length - 1)],
    [done, copy] = useCopied(),
    rows = lines(highlight(cur.code.replace(/\n$/, ""), cur.lang || lang));
  return React2.createElement(
    "div",
    {
      className: cx("tf-code", numbered && "tf-code--numbered", className),
      style: maxHeight
        ? { "--tf-code-max": typeof maxHeight == "number" ? `${maxHeight}px` : maxHeight }
        : void 0,
    },
    React2.createElement(
      "div",
      { className: "tf-code__bar" },
      React2.createElement(
        "div",
        { className: "tf-code__tabs", role: "tablist", "aria-label": "Files" },
        list.map((t, i) =>
          React2.createElement(
            "button",
            {
              key: t.name,
              type: "button",
              role: "tab",
              "aria-selected": i === at,
              className: "tf-code__tab",
              onClick: () => setAt(i),
            },
            t.name,
          ),
        ),
      ),
      React2.createElement(
        Button,
        { variant: "ghost", size: "sm", icon: done ? "check" : "copy", onClick: () => copy(cur.code) },
        done ? "Copied" : "Copy",
      ),
    ),
    React2.createElement(
      "div",
      { className: "tf-code__body", role: "tabpanel" },
      React2.createElement(
        "pre",
        null,
        React2.createElement(
          "code",
          null,
          rows.map((r, i) =>
            React2.createElement(
              "span",
              { key: i, className: "tf-code__line", "data-n": i + 1 },
              r.length ? r : " ",
            ),
          ),
        ),
      ),
    ),
    caption && React2.createElement("div", { className: "tf-code__caption" }, caption),
  );
}
var React3 = React,
  GLYPH = {
    proposed: "\u25CC",
    decided: "\u25A0",
    committed: "\u2713",
    rejected: "\u2715",
    held: "!",
    persisted: "\u25A3",
    said: "\u203A",
  };
function TraceLine({ n, task, state = "proposed", children, className }) {
  return React3.createElement(
    "div",
    { className: cx("tf-trace", `tf-trace--${state}`, className) },
    React3.createElement("span", { className: "tf-trace__n" }, n != null ? String(n).padStart(3, "0") : ""),
    React3.createElement(
      "span",
      { className: "tf-trace__glyph", "aria-hidden": "true" },
      GLYPH[state] || "\xB7",
    ),
    React3.createElement("span", { className: "tf-trace__task" }, task),
    React3.createElement("span", { className: "tf-trace__text" }, children),
  );
}
function Receipt({ title, children, code, revision, event, animate, className }) {
  return React3.createElement(
    "div",
    { className: cx("tf-block tf-receipt", className) },
    React3.createElement(
      Confirm,
      { detail: revision != null ? `rev ${revision}` : void 0, animate },
      "Confirmed",
    ),
    React3.createElement(
      "div",
      { className: "tf-block__head" },
      React3.createElement(Icon, { name: "receipt" }),
      title,
    ),
    children && React3.createElement("div", { className: "tf-block__body" }, children),
    React3.createElement(
      "div",
      { className: "tf-block__meta" },
      "event-backed",
      code && ` \xB7 ${code}`,
      event && ` \xB7 ${event}`,
    ),
  );
}
function Notice({ title, children, code, className }) {
  return React3.createElement(
    "div",
    { className: cx("tf-block tf-notice", className), role: "status" },
    React3.createElement(
      "div",
      { className: "tf-block__head" },
      React3.createElement(Icon, { name: "notice" }),
      title || children,
    ),
    title && children && React3.createElement("div", { className: "tf-block__body" }, children),
    code &&
      React3.createElement(
        "div",
        { className: "tf-block__meta" },
        React3.createElement("b", null, "notice"),
        " \xB7 ",
        code,
      ),
  );
}
function AnswerBlock({ label = "Answer", children, basis, className }) {
  return React3.createElement(
    "div",
    { className: cx("tf-block tf-answer", className) },
    React3.createElement(
      "div",
      { className: "tf-block__head" },
      label,
      basis &&
        React3.createElement(
          "span",
          { style: { textTransform: "none", letterSpacing: 0 } },
          "\xB7 on ",
          basis,
        ),
    ),
    React3.createElement("div", { className: "tf-block__body" }, children),
  );
}
function InteractionCard({
  title,
  body,
  entries,
  options = [],
  selected,
  hovered,
  status = "open",
  onSelect,
  id,
  layout = "column",
  tags = !0,
  className,
}) {
  let done = status !== "open" || selected != null;
  return React3.createElement(
    "div",
    {
      className: cx(
        "tf-card",
        done && "tf-card--resolved",
        status === "stale" && "tf-card--stale",
        className,
      ),
    },
    React3.createElement(
      "div",
      { className: "tf-card__head" },
      React3.createElement("span", { className: "tf-card__title" }, title),
      tags &&
        (status === "stale"
          ? React3.createElement(StateTag, { state: "refused" }, "Stale")
          : done
            ? React3.createElement(StateTag, { state: "decided" }, "Answered")
            : React3.createElement(StateTag, { state: "awaiting" })),
    ),
    body && React3.createElement("div", { className: "tf-card__body" }, body),
    entries &&
      entries.length > 0 &&
      React3.createElement(
        "dl",
        { className: "tf-card__entries" },
        entries.map((e) =>
          React3.createElement(
            "div",
            { key: e.label, className: "tf-card__entry" },
            React3.createElement("dt", null, e.label),
            React3.createElement(
              "dd",
              null,
              e.before != null &&
                React3.createElement(
                  React3.Fragment,
                  null,
                  React3.createElement("del", null, e.before),
                  " \u2192 ",
                ),
              e.after,
            ),
          ),
        ),
      ),
    React3.createElement(
      "div",
      { className: cx("tf-card__opts", layout === "row" && "tf-card__opts--row") },
      options.map((o, i) => {
        let label = typeof o == "string" ? o : o.label;
        return React3.createElement(
          "button",
          {
            key: label,
            type: "button",
            className: cx("tf-card__opt", o.primary && "tf-card__opt--primary"),
            "aria-pressed": selected === i,
            "data-hover": hovered === i,
            disabled: (done && selected !== i) || status === "stale",
            onClick: () => !done && onSelect && onSelect(i),
          },
          React3.createElement(
            "span",
            { className: "tf-card__box", "aria-hidden": "true" },
            selected === i && React3.createElement(Icon, { name: "check", size: 12 }),
          ),
          React3.createElement("span", null, label),
        );
      }),
    ),
    id && React3.createElement("div", { className: "tf-card__foot" }, "persisted \xB7 ", id),
  );
}
function Message({ role = "assistant", who, children, caret, claims, className }) {
  let name = who || (role === "user" ? "You" : role === "said" ? "Reading" : "Assistant");
  return React3.createElement(
    "div",
    { className: cx("tf-msg", `tf-msg--${role}`, className) },
    role !== "said" && React3.createElement("span", { className: "tf-msg__who" }, name),
    React3.createElement(
      "div",
      { className: "tf-msg__bubble" },
      role === "said" && "\u203A ",
      claims && typeof children == "string" ? withClaims(children, claims === "draw") : children,
      caret && React3.createElement("span", { className: "tf-caret", "aria-hidden": "true" }),
    ),
  );
}
var React4 = React,
  { useState: useState3, useEffect: useEffect2 } = React4;
function FrameMeter({
  cells,
  played,
  active,
  count = !0,
  total,
  legend,
  height = 22,
  animate,
  onSeek,
  label = "Frame meter",
  className,
}) {
  let reduced = useReducedMotion(),
    n = cells.length,
    [shown, setShown] = useState3(animate && !reduced ? 0 : n);
  useEffect2(() => {
    if (!animate || reduced) {
      setShown(n);
      return;
    }
    setShown(0);
    let i = 0,
      t = setInterval(() => {
        ((i += 1), setShown(i), i >= n && clearInterval(t));
      }, 38);
    return () => clearInterval(t);
  }, [animate, reduced, n]);
  let upto = played != null ? played : shown,
    // One slider, not a button per frame: a frame is a few pixels wide, too small to tap.
    seekAt = (event) => {
      let box = event.currentTarget.getBoundingClientRect();
      onSeek(Math.min(n, Math.max(1, Math.ceil(((event.clientX - box.left) / box.width) * n))));
    };
  return React4.createElement(
    "div",
    { className: cx("tf tf-fm", className), style: { "--h": `${height}px` } },
    React4.createElement(
      "div",
      { className: "tf-fm__row" },
      React4.createElement(
        "div",
        {
          className: cx("tf-fm__cells", onSeek && "tf-fm__cells--seek"),
          style: { "--n": n },
          role: onSeek ? "slider" : "img",
          "aria-label": label,
          ...(onSeek
            ? { tabIndex: 0, "aria-valuemin": 0, "aria-valuemax": n, "aria-valuenow": upto, onClick: seekAt }
            : {}),
        },
        cells.map((c, i) => {
          let k = typeof c == "string" ? c : c.k;
          return React4.createElement("span", {
            key: i,
            className: "tf-fm__cell",
            "data-k": k,
            "data-dim": i >= upto,
            "data-on": active === i,
            title: typeof c == "string" ? void 0 : c.title,
          });
        }),
      ),
      count &&
        React4.createElement(
          "span",
          { className: "tf-fm__count" },
          "F",
          String(Math.min(upto, n)).padStart(3, "0"),
          React4.createElement("span", null, "/", String(total || n).padStart(3, "0")),
        ),
    ),
    legend &&
      React4.createElement(
        "div",
        { className: "tf-fm__legend", style: { "--n": n } },
        legend.map((l) =>
          React4.createElement(
            "span",
            { key: l.label, style: { gridColumn: `span ${l.span}` } },
            React4.createElement("b", null, l.label),
            l.note,
          ),
        ),
      ),
  );
}
var HERO_TURN = [
  ...Array(7).fill("input"),
  "whiff",
  ...Array(10).fill("input"),
  ...Array(5).fill("decide"),
  ...Array(3).fill("commit"),
  ...Array(3).fill("confirm"),
  ...Array(2).fill("out"),
];
function EffortSettings({ defaultValue = "medium", presets, pricing, foot, className }) {
  let [v, setV] = useState3(defaultValue),
    p = presets[v],
    row = (k, val, note) =>
      React4.createElement(
        "div",
        { className: "tf-fx__row" },
        React4.createElement("span", null, k),
        React4.createElement("b", null, val),
        note && React4.createElement("small", null, note),
      ),
    locked = (k) =>
      React4.createElement(
        "div",
        { className: "tf-fx__row tf-fx__row--lock" },
        React4.createElement("span", null, React4.createElement(Icon, { name: "lock", size: 13 }), k),
        React4.createElement(StateTag, { state: "committed" }, "Identical"),
      );
  return React4.createElement(
    "div",
    { className: cx("tf tf-fx", className) },
    React4.createElement(
      "div",
      { className: "tf-fx__head" },
      React4.createElement("span", null, "Settings \xB7 effort preset"),
      React4.createElement(
        "div",
        { className: "tf-fx__presets", role: "radiogroup", "aria-label": "Effort preset" },
        ["low", "medium", "high"].map((id) =>
          React4.createElement(
            "button",
            { key: id, type: "button", role: "radio", "aria-checked": v === id, onClick: () => setV(id) },
            id,
          ),
        ),
      ),
    ),
    React4.createElement(
      "div",
      { className: "tf-fx__body" },
      React4.createElement(
        "div",
        { className: "tf-fx__col" },
        React4.createElement(
          "div",
          { className: "tf-fx__cap" },
          "Reading \xB7 probabilistic \xB7 on a mini model",
        ),
        p.measured
          ? [
              row("Live corpus passed", p.rate, `${p.passed} samples`),
              row("Model calls per item", p.calls, "an item is one turn, or a conversation of several"),
              row("Cost per item", p.cost, pricing),
              row("Turn latency, p50", p.p50),
            ].map((node, key) => React4.cloneElement(node, { key }))
          : React4.createElement(
              "div",
              { className: "tf-fx__row" },
              React4.createElement("span", null, "Not measured in this release"),
              React4.createElement(StateTag, { state: "neutral" }, "No figure"),
            ),
        React4.createElement("p", { className: "tf-fx__does" }, p.does),
      ),
      React4.createElement(
        "div",
        { className: "tf-fx__col" },
        React4.createElement(
          "div",
          { className: "tf-fx__cap" },
          "Effects \xB7 by construction \xB7 any model",
        ),
        locked("Side effects"),
        locked("Claims in the reply"),
        locked("Workflow state"),
        locked("Cards, policy, revisions"),
        React4.createElement(
          "p",
          { className: "tf-fx__does" },
          "Code reads no level. A low turn that misreads a message still cannot run a command its policy forbids, claim what no event backs, or leave a record in a state its workflow cannot express.",
        ),
      ),
    ),
    React4.createElement(
      "div",
      { className: "tf-fx__foot" },
      foot,
    ),
  );
}
var React5 = React,
  PHASES = ["Received", "Interpreted", "Reduced", "Executing", "Committed", "Composed", "Delivered"];
function StepStream({ phase = 0, steps = [], mode = "said", live, open = !0, onToggle, className }) {
  let shown = mode === "off" ? [] : steps;
  return React5.createElement(
    "div",
    { className: cx("tf-stream", live && "tf-stream--live", className) },
    React5.createElement(
      "button",
      {
        type: "button",
        className: "tf-stream__head",
        onClick: onToggle,
        "aria-expanded": open,
        disabled: !onToggle,
      },
      React5.createElement("span", { className: "tf-stream__dot", "aria-hidden": "true" }),
      React5.createElement(
        "span",
        { className: "tf-stream__title" },
        live ? "Streaming" : `Read in ${steps.length} step${steps.length === 1 ? "" : "s"}`,
      ),
      React5.createElement("span", { className: "tf-stream__phase" }, PHASES[phase]),
      onToggle && React5.createElement(Icon, { name: open ? "up" : "down", size: 14 }),
    ),
    open &&
      React5.createElement(
        "div",
        { className: "tf-stream__body" },
        React5.createElement(
          "ol",
          { className: "tf-stream__phases", "aria-label": "Turn phases" },
          PHASES.map((p, i) =>
            React5.createElement(
              "li",
              { key: p, "data-s": i < phase ? "done" : i === phase ? "on" : "off" },
              p,
            ),
          ),
        ),
        shown.length > 0 &&
          React5.createElement(
            "ul",
            { className: "tf-stream__steps" },
            shown.map((s, i) =>
              React5.createElement(
                "li",
                { key: i, className: cx("tf-in", s.rejected && "tf-stream__step--again") },
                React5.createElement("span", { "aria-hidden": "true" }, s.rejected ? "\u21BA" : "\u25CC"),
                React5.createElement(
                  "span",
                  null,
                  (mode === "said" && s.said) || s.text,
                  live &&
                    i === shown.length - 1 &&
                    React5.createElement("span", { className: "tf-caret", "aria-hidden": "true" }),
                ),
              ),
            ),
          ),
        mode === "off" &&
          React5.createElement(
            "div",
            { className: "tf-stream__note" },
            "Steps hidden: this surface's StepSink shows none. Phases still stream.",
          ),
        mode !== "off" &&
          steps.length === 0 &&
          React5.createElement(
            "div",
            { className: "tf-stream__note" },
            "No understanding steps: a click costs no model call.",
          ),
      ),
  );
}
var STAGES = [
    {
      id: "segment",
      band: "propose",
      q: "Which units the message holds: requests, questions, values, corrections, chitchat.",
    },
    { id: "coverage", band: "propose", q: "Whether a request or a question was missed." },
    {
      id: "take_up",
      band: "propose",
      q: "Which offer of the last reply a request takes up, or whether it declines them.",
    },
    { id: "route", band: "propose", q: "Which offered operation each request asks for, one act each." },
    { id: "locate", band: "propose", q: "Which record it is about, when more than one could be." },
    { id: "extract", band: "propose", q: "Each value, pointed at in the user\u2019s own words." },
    { id: "verify", band: "propose", q: "Whether the act matches what the user said." },
    { id: "reduce", band: "decide", q: "Which typed commands the whole turn compiles to, under policy." },
    { id: "commit", band: "claim", q: "What the ledger records, at an expected revision." },
    { id: "compose", band: "claim", q: "What the reply may claim: only what committed events back." },
  ];
var React6 = React,
  {
    useState: useState4,
    useEffect: useEffect3,
    useRef: useRef2,
    useMemo,
    useCallback: useCallback2,
  } = React6,
  RANK = { low: 0, medium: 1, high: 2 };
function compileTurn(scn, effort, fault) {
  let items = [];
  for (let e of scn.turn)
    if (!(e.min && RANK[effort] < RANK[e.min]) && e.k !== "said") {
      if (e.k === "user") {
        (items.push({ k: "type", text: e.text }), items.push(e));
        continue;
      }
      (e.fault &&
        fault &&
        items.push({
          k: "step",
          stage: e.stage,
          task: e.task,
          state: "rejected",
          text: e.fault,
          said: e.said ? "Reading that again: the first answer did not fit the message." : void 0,
        }),
        items.push(e.fault && fault ? { ...e, task: `${e.task}\xB72` } : e));
    }
  return items;
}
function hold(it) {
  switch (it.k) {
    case "type":
      return 350 + it.text.length * 17;
    case "user":
      return 650;
    case "said":
      return 520;
    case "step":
      return it.state === "rejected" ? 1100 : it.state === "decided" ? 760 : 560;
    case "event":
      return 640;
    case "block":
      return it.type === "card" ? 900 : 720;
    case "reply":
      return 1400;
    case "spent":
      return 1500;
    case "hover":
      return 650;
    case "click":
      return it.again ? 700 : 800;
    default:
      return 500;
  }
}
function useTyped(text, active, unit, ms) {
  let parts = useMemo(() => (unit === "word" ? text.split(/(\s+)/) : Array.from(text)), [text, unit]),
    [n, setN] = useState4(active ? 0 : parts.length);
  return (
    useEffect3(() => {
      if (!active) {
        setN(parts.length);
        return;
      }
      setN(0);
      let i = 0,
        t = setInterval(() => {
          ((i += 1), setN(i), i >= parts.length && clearInterval(t));
        }, ms);
      return () => clearInterval(t);
    }, [active, parts, ms]),
    [parts.slice(0, n).join(""), n < parts.length]
  );
}
var CLAIM_RE = /\[([^\]|]+)\|\d+\]/g;
function Rail({ seen, on }) {
  return React6.createElement(
    "div",
    { className: "tf-rail", role: "group", "aria-label": "Pipeline progress" },
    STAGES.map((s) =>
      React6.createElement(
        "div",
        {
          key: s.id,
          className: "tf-rail__s",
          "data-band": s.band,
          "data-state": on === s.id ? "on" : seen.has(s.id) ? "done" : "off",
        },
        React6.createElement("span", { className: "tf-rail__bar" }),
        React6.createElement("span", { className: "tf-rail__name" }, s.id),
      ),
    ),
  );
}
function Working({ text }) {
  return React6.createElement(
    "div",
    { className: "tf-working tf-in", role: "status", "aria-live": "polite" },
    React6.createElement(
      "span",
      { className: "tf-working__dots", "aria-hidden": "true" },
      React6.createElement("i", null),
      React6.createElement("i", null),
      React6.createElement("i", null),
    ),
    React6.createElement(
      "span",
      { key: text || "idle", className: "tf-working__text tf-in" },
      text || "Working on it",
    ),
  );
}
function Composer({ text, active, speed }) {
  let [shown, going] = useTyped(text || "", !!(text && active), "char", 15 / Number(speed));
  return React6.createElement(
    "div",
    { className: "tf-composer", "aria-hidden": "true" },
    React6.createElement(
      "span",
      { className: cx("tf-composer__field", !text && "tf-composer__field--empty") },
      text ? shown : "Message the travel desk\u2026",
      text && going && React6.createElement("span", { className: "tf-caret" }),
    ),
    React6.createElement(
      "span",
      { className: cx("tf-composer__send", text && !going && "tf-composer__send--ready") },
      React6.createElement(Icon, { name: "arrow", size: 14 }),
    ),
  );
}
var STATUS_TEXT = {
  idle: "Idle",
  reading: "Input",
  deciding: "Deciding",
  committed: "Confirmed",
  awaiting: "Your move",
};
function TurnPlayer({
  scenarios = SCENARIOS,
  initial = 0,
  autoPlay = !0,
  loop = !0,
  height = 500,
  defaultEffort = "medium",
  defaultView = "split",
  defaultSteps = "said",
  defaultTone = "neutral",
  className,
}) {
  let reduced = useReducedMotion(),
    [si, setSi] = useState4(initial),
    [effort, setEffort] = useState4(defaultEffort),
    [fault, setFault] = useState4(!1),
    [stepsMode, setStepsMode] = useState4(defaultSteps === "describe" ? "said" : defaultSteps),
    [overlay, setOverlay] = useState4(!1),
    [tone, setTone] = useState4(defaultTone),
    [streamOpen, setStreamOpen] = useState4(null),
    [view, setView] = useState4(defaultView),
    [speed, setSpeed] = useState4("1"),
    [playing, setPlaying] = useState4(autoPlay),
    [cursor, setCursor] = useState4(0),
    [alt, setAlt] = useState4(null),
    [touched, setTouched] = useState4(!1),
    [visible, setVisible] = useState4(!0),
    root = useRef2(null),
    chat = useRef2(null),
    log = useRef2(null),
    scn = scenarios[si],
    items = useMemo(() => compileTurn(scn, effort, fault), [scn, effort, fault]),
    n = items.length,
    saidCount = items.filter((x) => x.k === "step" && x.said).length;
  (useEffect3(() => {
    reduced && (setPlaying(!1), setCursor(n));
  }, [reduced, n]),
    useEffect3(() => {
      if (!root.current || !("IntersectionObserver" in window)) return;
      let io = new IntersectionObserver(([e]) => setVisible(e.isIntersecting), { threshold: 0.25 });
      return (io.observe(root.current), () => io.disconnect());
    }, []));
  let restart = useCallback2(
      (play = !0) => {
        (setCursor(0), setAlt(null), setStreamOpen(null), setPlaying(play && !reduced));
      },
      [reduced],
    ),
    touch = () => setTouched(!0);
  (useEffect3(() => {
    if (!playing || !visible || alt != null) return;
    if (cursor >= n) {
      if (!loop || touched) {
        setPlaying(!1);
        return;
      }
      let t2 = setTimeout(() => {
        (setSi((i) => (i + 1) % scenarios.length), setCursor(0));
      }, 4200);
      return () => clearTimeout(t2);
    }
    let wait = cursor === 0 ? 450 : hold(items[cursor - 1]),
      t = setTimeout(() => setCursor((c) => c + 1), wait / Number(speed));
    return () => clearTimeout(t);
  }, [playing, visible, cursor, n, items, speed, loop, touched, alt, scenarios.length]),
    useEffect3(() => {
      [chat.current, log.current].forEach((el) => {
        el && (el.scrollTop = el.scrollHeight);
      });
    }, [cursor, alt, view]));
  let applied = items.slice(0, cursor),
    seen = new Set(),
    on = null,
    status = "idle",
    chatNodes = [],
    logNodes = [],
    ledger = [],
    cards = {},
    lastCard = null,
    hasEvent = !1,
    openCard = !1,
    line = 0,
    lastRev = null,
    phase = -1,
    composer = null,
    working = !1,
    workingText = null;
  (applied.forEach((it, i) => {
    let animate = i === applied.length - 1 && playing && !reduced;
    if (
      (it.k === "type" && (composer = { text: it.text, active: animate }),
      it.k === "user" &&
        ((composer = null),
        (phase = 0),
        (status = "reading"),
        (working = !0),
        (workingText = null),
        chatNodes.push(
          React6.createElement(Message, { key: i, role: "user", className: "tf-in" }, it.text),
        )),
      it.k === "step" &&
        (on && seen.add(on),
        (on = it.stage),
        (status = it.state === "decided" || it.state === "held" ? "deciding" : "reading"),
        it.stage === "reduce"
          ? (phase = Math.max(phase, 2))
          : it.stage === "commit"
            ? (phase = Math.max(phase, 3))
            : it.stage !== "compose" && (phase = Math.max(phase, 0)),
        working && it.said && stepsMode === "said" && (workingText = it.said),
        (line += 1),
        logNodes.push(
          React6.createElement(
            TraceLine,
            { key: i, n: line, task: it.task, state: it.state || "proposed", className: "tf-in" },
            it.text,
          ),
        )),
      it.k === "event" &&
        (on && seen.add(on),
        (line += 1),
        it.state === "persisted"
          ? ledger.push(
              React6.createElement(
                TraceLine,
                { key: i, n: line, task: "persisted", state: "persisted", className: "tf-in" },
                it.code,
                " \xB7 ",
                it.id,
              ),
            )
          : ((on = "commit"),
            (hasEvent = !0),
            (status = "committed"),
            (phase = Math.max(phase, 4)),
            (lastRev = { rev: it.rev, fresh: animate }),
            ledger.push(
              React6.createElement(
                TraceLine,
                { key: i, n: line, task: `rev ${it.rev}`, state: "committed", className: "tf-in" },
                it.code,
                " \xB7 ",
                it.id,
              ),
            ))),
      (it.k === "block" || it.k === "reply") && ((working = !1), (phase = Math.max(phase, 5))),
      it.k === "block")
    ) {
      if (it.type === "receipt")
        if ((on && seen.add(on), (on = "compose"), overlay))
          chatNodes.push(
            React6.createElement(
              Receipt,
              { key: i, className: "tf-in", animate, title: it.title, code: it.code, revision: it.rev },
              it.body,
            ),
          );
        else {
          let last = chatNodes[chatNodes.length - 1];
          last && last.done ? last.done.push(it) : chatNodes.push({ done: [it], key: i });
        }
      (it.type === "notice" &&
        chatNodes.push(
          React6.createElement(
            Notice,
            { key: i, className: "tf-in", code: overlay ? it.code : void 0 },
            it.text,
          ),
        ),
        it.type === "answer" &&
          chatNodes.push(
            React6.createElement(
              AnswerBlock,
              { key: i, className: "tf-in", basis: overlay ? it.basis : void 0 },
              it.text,
            ),
          ),
        it.type === "card" &&
          ((cards[i] = { selected: null, hovered: null }),
          it.status !== "stale" && ((lastCard = i), (openCard = !0), (status = "awaiting")),
          chatNodes.push({ card: i, it })));
    }
    if (
      (it.k === "reply" &&
        (on && seen.add(on),
        (on = "compose"),
        chatNodes.push(
          React6.createElement(
            Message,
            {
              key: `${i}-${tone}-${overlay}`,
              role: "assistant",
              claims: overlay ? (animate ? "draw" : !0) : void 0,
              className: "tf-in",
            },
            overlay
              ? (it.tones && it.tones[tone]) || it.text
              : ((it.tones && it.tones[tone]) || it.text).replace(CLAIM_RE, "$1"),
          ),
        )),
      it.k === "spent")
    ) {
      let [calls, tokens] = scn.spent[effort];
      logNodes.push(
        React6.createElement(
          "div",
          { key: i, className: "tf-player__spent tf-in" },
          "spent ",
          React6.createElement(
            "b",
            null,
            calls + (fault ? 1 : 0) + (stepsMode === "said" ? saidCount : 0),
            " calls",
          ),
          " on a ",
          React6.createElement("b", null, "mini"),
          " model",
          stepsMode === "said" && saidCount ? ` (${saidCount} for StepSaid)` : "",
          " \xB7 ",
          (tokens + (fault ? 410 : 0)).toLocaleString("en-GB"),
          " prompt tokens \xB7 effort ",
          React6.createElement("b", null, effort),
          " \xB7 scripted",
        ),
      );
    }
    if (
      (it.k === "hover" && lastCard != null && (cards[lastCard].hovered = it.option),
      it.k === "click" && lastCard != null)
    ) {
      ((cards[lastCard] = { selected: it.option, hovered: null }),
        (openCard = !1),
        (working = !0),
        (workingText = null),
        (phase = Math.max(phase, 0)));
      let label = items[lastCard].options[it.option];
      it.again ||
        chatNodes.push(
          React6.createElement(
            Message,
            { key: i, role: "user", className: "tf-in tf-msg--choice" },
            typeof label == "string" ? label : label.label,
          ),
        );
    }
  }),
    alt != null &&
      lastCard != null &&
      ((cards[lastCard] = { selected: alt, hovered: null }), (openCard = !1), (working = !1)),
    cursor >= n && on && (seen.add(on), (on = null)),
    cursor >= n && n && ((phase = 6), (working = !1)),
    (cursor >= n || alt != null) &&
      (status = openCard ? "awaiting" : hasEvent ? "committed" : alt != null ? "idle" : status));
  let onPick = (idx, it, cardIndex) => {
      if ((touch(), it.next && idx === 0)) {
        pickScenario((si + 1) % scenarios.length);
        return;
      }
      if (it.pick != null)
        if (idx === it.pick) {
          let target = items.findIndex((x, j) => j > cardIndex && x.k === "click");
          target >= 0 && (setCursor(target + 1), setPlaying(!reduced));
        } else
          (setAlt(idx),
            setCursor(items.findIndex((x, j) => j > cardIndex && x.k === "hover")),
            setPlaying(!1));
    },
    renderedChat = chatNodes.map((node) => {
      if (node && node.done)
        return React6.createElement(
          "div",
          { key: `done-${node.key}`, className: "tf-done tf-in" },
          node.done.map((r) =>
            React6.createElement(
              "div",
              { key: r.title, className: "tf-done__row" },
              React6.createElement(Icon, { name: "check", size: 14 }),
              React6.createElement(
                "span",
                null,
                React6.createElement("b", null, r.title),
                " \xB7 ",
                r.body,
              ),
            ),
          ),
        );
      if (!node || node.card == null) return node;
      let { it } = node,
        st = cards[node.card];
      return React6.createElement(InteractionCard, {
        key: node.card,
        className: "tf-in",
        status: it.status || "open",
        tags: overlay,
        id: overlay ? it.id : void 0,
        title: it.title,
        body: it.body,
        entries: it.entries,
        options: it.options,
        layout: it.layout,
        selected: st.selected,
        hovered: st.hovered,
        onSelect: (idx) => onPick(idx, it, node.card),
      });
    });
  (working &&
    playing &&
    cursor < n &&
    renderedChat.push(React6.createElement(Working, { key: "working", text: workingText })),
    alt != null &&
      renderedChat.push(
        React6.createElement(
          Notice,
          { key: "alt", className: "tf-in", code: overlay ? "interaction.declined" : void 0 },
          "You chose not to go ahead. Nothing changed.",
        ),
      ));
  let frames = items.map((it) =>
      it.k === "step"
        ? it.state === "rejected"
          ? "whiff"
          : it.state === "decided" || it.state === "held"
            ? "decide"
            : "input"
        : it.k === "event"
          ? it.state === "persisted"
            ? "card"
            : "commit"
          : it.k === "block"
            ? it.type === "receipt"
              ? "confirm"
              : it.type === "card"
                ? "card"
                : "out"
            : it.k === "reply"
              ? "confirm"
              : it.k === "user" || it.k === "said" || it.k === "type"
                ? "out"
                : "empty",
    ),
    seek = (to) => {
      (touch(), setAlt(null), setPlaying(!1), setCursor(to));
    },
    onScrubKey = (e) => {
      (e.key === "ArrowRight" || e.key === "ArrowLeft") &&
        (e.preventDefault(),
        touch(),
        setPlaying(!1),
        setCursor((c) => Math.max(0, Math.min(n, c + (e.key === "ArrowRight" ? 1 : -1)))));
    },
    pickScenario = (i) => {
      (touch(), setSi(i), setCursor(0), setAlt(null), setPlaying(!reduced));
    };
  return React6.createElement(
    "div",
    {
      ref: root,
      className: cx("tf tf-player", view === "user" && "tf-player--user", className),
      style: { "--tf-player-h": `${height}px` },
    },
    React6.createElement(
      "div",
      { className: "tf-player__tabs", role: "tablist", "aria-label": "Scenarios" },
      scenarios.map((s, i) =>
        React6.createElement(
          "button",
          {
            key: s.id,
            type: "button",
            role: "tab",
            "aria-selected": i === si,
            className: "tf-player__tab",
            onClick: () => pickScenario(i),
          },
          React6.createElement("span", null, String(i + 1).padStart(2, "0")),
          s.label,
        ),
      ),
    ),
    React6.createElement(
      "div",
      { className: "tf-player__win" },
      React6.createElement(
        "div",
        { className: "tf-player__head" },
        React6.createElement(
          "div",
          null,
          React6.createElement(
            "span",
            { className: "tf-rec", "data-live": playing && cursor < n },
            "Replay",
          ),
          React6.createElement(
            "span",
            { className: "tf-fcount" },
            "F",
            String(Math.min(cursor, n)).padStart(3, "0"),
          ),
        ),
        React6.createElement(
          "div",
          null,
          React6.createElement(
            "span",
            { className: "tf-hide-sm" },
            "Turn ",
            String(scn.ctx.turn).padStart(2, "0"),
            " \xB7",
          ),
          React6.createElement("b", null, scn.ctx.record),
        ),
        React6.createElement(
          "div",
          { className: "tf-status", "data-s": status, "aria-live": "polite" },
          React6.createElement("span", { className: "tf-status__dot" }),
          STATUS_TEXT[status],
        ),
      ),
      React6.createElement(
        "div",
        { className: "tf-player__config", role: "group", "aria-label": "Turn configuration" },
        React6.createElement(
          "span",
          { className: "tf-player__cfg" },
          React6.createElement("code", null, "effort"),
          React6.createElement(Segmented, {
            label: "Effort",
            value: effort,
            onChange: (v) => {
              (touch(), setEffort(v), restart(!0));
            },
            options: ["low", "medium", "high"],
          }),
        ),
        React6.createElement(
          "span",
          { className: "tf-player__cfg" },
          React6.createElement("code", null, "steps"),
          React6.createElement(Segmented, {
            label: "Progress shown to the user",
            value: stepsMode,
            onChange: (v) => {
              (touch(), setStepsMode(v));
            },
            options: [
              {
                value: "said",
                label: "said",
                title:
                  "NarrationConfig::steps: each step said in the user\u2019s language, shown as one status line",
              },
              {
                value: "off",
                label: "off",
                title: "No step lines: the user sees a plain working indicator",
              },
            ],
          }),
        ),
        React6.createElement(
          "span",
          { className: "tf-player__cfg" },
          React6.createElement("code", null, "tone"),
          React6.createElement(Segmented, {
            label: "Reply tone",
            value: tone,
            onChange: (v) => {
              (touch(), setTone(v));
            },
            options: ["neutral", "warm", "formal", "concise"],
          }),
        ),
        React6.createElement(
          "span",
          { className: "tf-player__cfg" },
          React6.createElement("code", null, "overlay"),
          React6.createElement(
            "span",
            { className: "tf-seg" },
            React6.createElement(
              "button",
              {
                type: "button",
                className: "tf-seg__opt",
                "aria-pressed": overlay,
                title: "Show claim highlights, revisions and codes inside the chat",
                onClick: () => {
                  (touch(), setOverlay((o) => !o));
                },
              },
              React6.createElement(Icon, { name: "split", size: 13 }),
              overlay ? "debug" : "off",
            ),
          ),
        ),
        React6.createElement(
          "span",
          { className: "tf-player__cfg" },
          React6.createElement("code", null, "fault"),
          React6.createElement(
            "span",
            { className: "tf-seg" },
            React6.createElement(
              "button",
              {
                type: "button",
                className: "tf-seg__opt",
                "aria-pressed": fault,
                title: "Make one model answer malformed",
                onClick: () => {
                  (touch(), setFault((f) => !f), restart(!0));
                },
              },
              React6.createElement(Icon, { name: "bolt", size: 13 }),
              fault ? "on" : "off",
            ),
          ),
        ),
      ),
      React6.createElement(
        "div",
        { className: "tf-player__panes" },
        React6.createElement(
          "div",
          { className: "tf-player__user" },
          React6.createElement(
            "div",
            { className: "tf-player__panecap" },
            React6.createElement("span", null, "What your user sees"),
          ),
          React6.createElement(
            "div",
            { ref: chat, className: "tf-player__chat", role: "log", "aria-label": "What the user sees" },
            renderedChat,
          ),
          React6.createElement(Composer, {
            text: composer && composer.text,
            active: composer && composer.active,
            speed,
          }),
        ),
        React6.createElement(
          "div",
          { className: "tf-player__rt", role: "region", "aria-label": "What the runtime did" },
          React6.createElement(
            "div",
            { className: "tf-player__rthead" },
            React6.createElement(
              "div",
              { className: "tf-player__rtlabel" },
              React6.createElement("span", null, "Inspector \xB7 for developers"),
              React6.createElement("span", null, Math.min(cursor, n), "/", n),
            ),
            React6.createElement(
              "ol",
              { className: "tf-player__phases", "aria-label": "TurnEvent phases" },
              PHASES.map((p, i) =>
                React6.createElement(
                  "li",
                  { key: p, "data-s": phase < 0 ? "off" : i < phase ? "done" : i === phase ? "on" : "off" },
                  p,
                ),
              ),
            ),
            React6.createElement(Rail, { seen, on }),
          ),
          React6.createElement(
            "div",
            { className: "tf-player__cols", "aria-hidden": "true" },
            React6.createElement("span", null, "#"),
            React6.createElement("span", null),
            React6.createElement("span", null, "Task"),
            React6.createElement("span", null, "Entry"),
          ),
          React6.createElement(
            "div",
            { ref: log, className: "tf-player__log" },
            logNodes.length
              ? logNodes
              : React6.createElement("div", { className: "tf-player__empty" }, "Waiting for a message."),
          ),
          React6.createElement(
            "div",
            { className: "tf-player__ledger" },
            React6.createElement(
              "div",
              { className: "tf-player__rtlabel" },
              React6.createElement("span", null, "Ledger \xB7 confirmed frames"),
              React6.createElement("span", null, "no confirm, no claim"),
            ),
            ledger.length
              ? ledger
              : React6.createElement(
                  "div",
                  { className: "tf-player__empty" },
                  "No committed events. Nothing may be claimed.",
                ),
            lastRev &&
              React6.createElement(
                Confirm,
                { key: lastRev.rev, animate: lastRev.fresh, detail: `rev ${lastRev.rev}` },
                "Confirmed",
              ),
          ),
        ),
      ),
      React6.createElement(
        "div",
        { className: "tf-player__fm", onKeyDown: onScrubKey },
        React6.createElement(FrameMeter, {
          cells: frames,
          played: Math.min(cursor, n),
          active: cursor > 0 ? Math.min(cursor, n) - 1 : null,
          onSeek: seek,
          height: 16,
          label: "Replay timeline, one cell per frame",
        }),
      ),
      React6.createElement(
        "div",
        { className: "tf-player__controls" },
        React6.createElement(
          "div",
          { className: "tf-player__transport" },
          React6.createElement(Button, {
            variant: "ghost",
            size: "sm",
            icon: playing && cursor < n ? "pause" : "play",
            label: playing && cursor < n ? "Pause" : "Play",
            onClick: () => {
              (touch(), cursor >= n || alt != null ? restart(!0) : setPlaying((p) => !p));
            },
          }),
          React6.createElement(Button, {
            variant: "ghost",
            size: "sm",
            icon: "step",
            label: "Next step",
            onClick: () => {
              (touch(), setPlaying(!1), setAlt(null), setCursor((c) => Math.min(n, c + 1)));
            },
          }),
          React6.createElement(Button, {
            variant: "ghost",
            size: "sm",
            icon: "restart",
            label: "Restart",
            onClick: () => {
              (touch(), restart(!0));
            },
          }),
        ),
        React6.createElement(
          "div",
          { className: "tf-player__ctl" },
          "View",
          React6.createElement(Segmented, {
            label: "View",
            value: view,
            onChange: (v) => {
              (touch(), setView(v));
            },
            options: [
              { value: "split", label: "inspector", icon: "split" },
              { value: "user", label: "user only", icon: "user" },
            ],
          }),
        ),
        React6.createElement(Segmented, {
          label: "Speed",
          value: speed,
          onChange: (v) => {
            (touch(), setSpeed(v));
          },
          options: [
            { value: "1", label: "1\xD7" },
            { value: "2", label: "2\xD7" },
          ],
        }),
      ),
    ),
    React6.createElement(
      "div",
      { className: "tf-player__note" },
      "Scripted replay \xB7 effort buys judgment, never authority: at any level, a misread message cannot run a command its policy forbids.",
    ),
  );
}
var React7 = React,
  { useState: useState5 } = React7;
function Section({ id, number, eyebrow, title, lead, rule = !0, children, className }) {
  return React7.createElement(
    "section",
    { id, className: cx("tf tf-section", rule && "tf-section--rule", className) },
    React7.createElement(
      "div",
      { className: "tf-section__inner" },
      (eyebrow || title) &&
        React7.createElement(
          "div",
          { className: "tf-section__head" },
          React7.createElement(
            "div",
            { className: "tf-eyebrow" },
            number && React7.createElement("b", null, number),
            eyebrow,
          ),
          React7.createElement(
            "div",
            null,
            title && React7.createElement("h2", { className: "tf-section__title" }, title),
            lead && React7.createElement("p", { className: "tf-section__lead" }, lead),
          ),
        ),
      children && React7.createElement("div", { className: "tf-section__body" }, children),
    ),
  );
}
function Manifesto({ className }) {
  let laws = [
      [
        "input",
        "Models propose meaning.",
        "Small tasks, one narrow question each, answers checked by code. Mini and flash models are enough.",
      ],
      [
        "decide",
        "Reducers decide effects.",
        "Typed commands with an expected revision and an idempotency key, under policy.",
      ],
      [
        "confirm",
        "Events decide claims.",
        "The reply says only what the ledger confirmed. No confirm, no claim.",
      ],
    ],
    glyph = { input: "\u25CC", decide: "\u25A0", confirm: "\u2713" };
  return React7.createElement(
    "section",
    { className: cx("tf tf-manifesto", className) },
    React7.createElement(
      "div",
      { className: "tf-manifesto__inner" },
      React7.createElement(
        "blockquote",
        null,
        "Let the model understand language. ",
        React7.createElement("span", null, "Let your code control reality."),
      ),
      React7.createElement(
        "div",
        { className: "tf-manifesto__laws" },
        laws.map(([k, t, v]) =>
          React7.createElement(
            "div",
            { key: t, className: "tf-manifesto__law" },
            React7.createElement(
              "b",
              null,
              React7.createElement("i", { "aria-hidden": "true" }, glyph[k]),
              t,
            ),
            v,
          ),
        ),
      ),
    ),
  );
}
var BANDS = [
  { id: "propose", title: "Models propose", tag: "proposed" },
  { id: "decide", title: "Code decides", tag: "decided" },
  { id: "claim", title: "Events confirm", tag: "committed" },
];
function PipelineRail({ className }) {
  let n = 0;
  return React7.createElement(
    "div",
    { className: cx("tf tf-pipe", className) },
    BANDS.map((b) => {
      let stages = STAGES.filter((s) => s.band === b.id);
      return React7.createElement(
        "div",
        { key: b.id, className: `tf-pipe__band tf-pipe__band--${b.id}` },
        React7.createElement(
          "div",
          { className: "tf-pipe__label" },
          React7.createElement("h3", null, b.title),
          b.id !== "decide" && React7.createElement(StateTag, { state: b.tag }),
        ),
        React7.createElement(
          "div",
          { className: "tf-pipe__stages", style: { "--n": stages.length } },
          stages.map(
            (s) => (
              (n += 1),
              React7.createElement(
                "div",
                { key: s.id, className: "tf-pipe__stage" },
                React7.createElement("span", { className: "tf-pipe__cell", "aria-hidden": "true" }),
                React7.createElement("span", { className: "tf-pipe__n" }, String(n).padStart(2, "0")),
                React7.createElement("span", { className: "tf-pipe__name" }, s.id),
                React7.createElement("span", { className: "tf-pipe__q" }, s.q),
              )
            ),
          ),
        ),
      );
    }),
  );
}
var SPEC = [
    {
      n: "1",
      title: "Side-effect integrity",
      what: "No write on the wrong case, over a newer revision, twice for one key, or from an ambiguous target.",
      by: "Reducer, command policy, ledger commit path",
      stamp: !0,
    },
    {
      n: "2",
      title: "Claim integrity",
      what: 'No "created", "changed" or "sent" without the event or receipt that proves it. Unknown outcomes stay unknown.',
      by: "Ledger, receipt composition",
      stamp: !0,
    },
    {
      n: "3",
      title: "Workflow-state consistency",
      what: "One lifecycle phase, projected purely from persisted state and the workflow version. The projector reads neither the clock nor the transcript.",
      by: "Pure projector, state exploration",
      stamp: !0,
    },
    {
      n: "4",
      title: "Semantic turn completion",
      what: "Mini models read one narrow question at a time, so they are sometimes wrong. This is the only place RNG lives.",
      by: "Checked tasks, votes, mandatory safe degradation",
      soft: !0,
      tag: "RNG \xB7 fails safe",
    },
    {
      n: "5",
      title: "Conversational quality",
      what: "Tone varies with the model. The reply is reviewed before it is shown and cannot contradict a receipt, and code makes it end on a way forward, offering only what the domain accepts now.",
      by: "Reply tasks, progress checks, simulated users",
      soft: !0,
      tag: "Product-dependent",
    },
  ],
  DEGRADE = [
    ["A question", "A missing value or an ambiguous record asks. The case is not touched."],
    ["An abstention", "No evidence, no command. The turn says so out loud."],
    ["A proposal", "Risky acts become a card. Nothing commits until the user answers."],
    ["A partial result", "Safe acts apply, the rest are held, and every act gets a result."],
  ];
function SpecSheet({ className }) {
  return React7.createElement(
    "div",
    { className: cx("tf", className) },
    React7.createElement(
      "table",
      { className: "tf-table" },
      React7.createElement(
        "thead",
        null,
        React7.createElement(
          "tr",
          null,
          React7.createElement("th", null, "\xA7"),
          React7.createElement("th", null, "Property"),
          React7.createElement("th", null, "Enforced by"),
          React7.createElement("th", { style: { textAlign: "right" } }, "Target"),
        ),
      ),
      React7.createElement(
        "tbody",
        null,
        SPEC.map((r) =>
          React7.createElement(
            "tr",
            { key: r.n, className: r.soft ? "tf-soft" : void 0 },
            React7.createElement("td", { className: "tf-n" }, "2.", r.n),
            React7.createElement("td", null, React7.createElement("b", null, r.title), r.what),
            React7.createElement("td", null, React7.createElement("code", null, r.by)),
            React7.createElement(
              "td",
              { className: "tf-stampcell" },
              r.stamp
                ? React7.createElement(Confirm, { detail: "effectively 100%" }, "By construction")
                : React7.createElement(StateTag, { state: r.n === "4" ? "proposed" : "neutral" }, r.tag),
            ),
          ),
        ),
      ),
    ),
    React7.createElement(
      "div",
      { className: "tf-degrade" },
      React7.createElement(
        "div",
        { className: "tf-degrade__cap" },
        "\u25CC When a reading misses, it can only recover into",
      ),
      DEGRADE.map(([k, v]) =>
        React7.createElement("div", { key: k }, React7.createElement("b", null, k), v),
      ),
    ),
  );
}
var CONTRAST = [
  [
    "Who performs a write",
    "The model calls a write tool",
    "A reducer compiles a typed command; policy decides",
  ],
  ["What the reply may claim", "Whatever the model writes", "Only what a committed event or receipt backs"],
  ["Two records match", "Usually the most recent one", "A selection card. Never a recency guess"],
  ["A double click, a retry, a crash", "Depends on each tool", "One idempotency key, one effect"],
  ["A misread message", "A wrong tool call, found later", "Checked against the user’s words; a risky act still waits for its card"],
  [
    "The model it needs",
    "Large, with a long context",
    "Small: no task needs more than a few hundred tokens",
  ],
];
function ContrastTable({
  rows = CONTRAST,
  left = "Model-calls-tools agents",
  right = "Turnframe",
  className,
}) {
  return React7.createElement(
    "table",
    { className: cx("tf tf-table tf-table--contrast", className) },
    React7.createElement(
      "thead",
      null,
      React7.createElement(
        "tr",
        null,
        React7.createElement(
          "th",
          { scope: "col" },
          React7.createElement("span", { className: "tf-sr" }, "Question"),
        ),
        React7.createElement("th", { scope: "col" }, left),
        React7.createElement("th", { scope: "col" }, right),
      ),
    ),
    React7.createElement(
      "tbody",
      null,
      rows.map((r) =>
        React7.createElement(
          "tr",
          { key: r[0] },
          React7.createElement("td", null, React7.createElement("b", null, r[0])),
          React7.createElement("td", null, r[1]),
          React7.createElement("td", null, r[2]),
        ),
      ),
    ),
  );
}
function CrateTable({ crates, className }) {
  let n = 0;
  return React7.createElement(
    "div",
    { className: cx("tf", className) },
    React7.createElement(
      "table",
      { className: "tf-table" },
      React7.createElement(
        "thead",
        null,
        React7.createElement(
          "tr",
          null,
          React7.createElement("th", null, "Part"),
          React7.createElement("th", null, "Crate"),
          React7.createElement("th", null, "Role"),
          React7.createElement("th", null, "Feature"),
        ),
      ),
      React7.createElement(
        "tbody",
        null,
        crates.map(([g, list]) =>
          React7.createElement(
            React7.Fragment,
            { key: g },
            React7.createElement(
              "tr",
              { className: "tf-group" },
              React7.createElement("td", { colSpan: 4 }, g),
            ),
            list.map(
              ([c, d, f]) => (
                (n += 1),
                React7.createElement(
                  "tr",
                  { key: c },
                  React7.createElement("td", { className: "tf-n" }, String(n).padStart(2, "0")),
                  React7.createElement("td", null, React7.createElement("code", null, c)),
                  React7.createElement("td", null, d),
                  React7.createElement("td", null, f === "included" ? React7.createElement("span", { className: "tf-faint" }, f) : React7.createElement("code", null, f)),
                )
              ),
            ),
          ),
        ),
      ),
    ),
    React7.createElement(
      "div",
      { className: "tf-flags" },
      React7.createElement("b", null, "Also"),
      React7.createElement("code", null, "all-providers"),
      React7.createElement("code", null, "full"),
      React7.createElement(
        "span",
        { className: "tf-faint", style: { fontSize: 13 } },
        "\xB7 turnframe-macros is reserved; no macros ship in 0.1",
      ),
    ),
  );
}
// The hostile refund: nine attacks on a refund, each a recording of the real runtime
// (examples/refund-desk). The first run is shown whole in the static page; picking an attack
// replays its frames station by station.
var BREAK_STATIONS = [
    ["reading", "Reading", "The model, scripted"],
    ["proposal", "Proposal", "What understanding hands over"],
    ["reducer", "Reducer", "Resolution, the domain, policy"],
    ["decision", "Decision", "Blocked, a card, or a commit"],
    ["ledger", "Ledger", "Events, receipts, the reply"],
  ],
  BREAK_GROUPS = [
    ["none", null],
    ["model", "The model"],
    ["user", "The user"],
    ["world", "The world"],
  ],
  BREAK_TASK = {
    message: "message",
    unit: "read as",
    act: "proposed",
    result: "result",
    policy: "policy",
    world: "meanwhile",
    notice: "notice",
    refused: "refused",
    receipt: "receipt",
    outbox: "outbox",
  };
function breakHold(frame) {
  return frame.kind === "card" || frame.kind === "click" ? 900 : frame.kind === "reply" ? 1100 : 480;
}
function BreakFrame({ frame, n, clicked, stale }) {
  if (frame.kind === "card")
    return React.createElement(InteractionCard, {
      className: "tf-in",
      status: stale ? "stale" : "open",
      title: frame.card.title,
      body: frame.card.body,
      entries: frame.card.entries,
      options: frame.card.options,
      layout: "row",
      selected: clicked == null ? void 0 : clicked,
      id: frame.id,
    });
  if (frame.kind === "reply")
    return React.createElement(
      Message,
      { className: "tf-in", role: "assistant", who: frame.scripted ? "Reply, in the scripted model's words" : "Reply" },
      frame.text,
    );
  let task =
      frame.kind === "event" ? `rev ${frame.rev}` : frame.kind === "click" ? "click" : BREAK_TASK[frame.kind] || frame.kind,
    state = frame.kind === "click" ? "decided" : frame.state || (frame.kind === "world" ? "held" : "proposed"),
    text = frame.kind === "event" ? `${frame.code} \xB7 ${frame.id}` : frame.text;
  return React.createElement(
    TraceLine,
    { n, task, state, className: cx("tf-in", frame.scripted && "tf-break__scripted") },
    text,
    frame.scripted && React.createElement("span", { className: "tf-break__tag" }, "scripted"),
  );
}
function BreakIt({ recording, className }) {
  let runs = recording.runs,
    reduced = useReducedMotion(),
    [ri, setRi] = useState(0),
    run = runs[ri],
    n = run.frames.length,
    [cursor, setCursor] = useState(n),
    [playing, setPlaying] = useState(!1),
    choose = useCallback((i, play) => {
      setRi(i);
      setCursor(play ? 0 : runs[i].frames.length);
      setPlaying(!!play);
      if (typeof history != "undefined" && history.replaceState) history.replaceState(null, "", `#break-${runs[i].id}`);
    }, [runs]);
  useEffect(() => {
    let wanted = typeof location != "undefined" && location.hash.startsWith("#break-") ? location.hash.slice(7) : null,
      at = wanted ? runs.findIndex((r) => r.id === wanted) : -1;
    at > 0 && choose(at, !1);
  }, []);
  useEffect(() => {
    if (reduced && playing) {
      setCursor(n);
      setPlaying(!1);
    }
  }, [reduced, playing, n]);
  useEffect(() => {
    if (!playing) return;
    if (cursor >= n) {
      setPlaying(!1);
      return;
    }
    let t = setTimeout(() => setCursor((c) => c + 1), cursor === 0 ? 300 : breakHold(run.frames[cursor - 1]));
    return () => clearTimeout(t);
  }, [playing, cursor, n, run]);
  let shown = run.frames.slice(0, cursor),
    done = cursor >= n,
    current = shown.length ? shown[shown.length - 1].station : null,
    clicks = {},
    stale = {},
    clicked = null;
  // A click the runtime refused whole leaves its card stale.
  shown.forEach((f) => {
    if (f.kind === "click" && f.card) {
      clicks[f.id] = f.card.options.findIndex((o) => o.id === f.option);
      clicked = f.id;
    }
    if (f.kind === "refused" && clicked) stale[clicked] = !0;
    if (f.kind === "card") clicked = null;
  });
  let stations = BREAK_STATIONS.map(([key, name, what], si) => {
    let mine = shown.map((f, i) => [f, i]).filter(([f]) => f.station === key),
      stopped = done && run.stopped_at === key,
      reached = mine.length > 0,
      events = key === "ledger" && done && !shown.some((f) => f.kind === "event");
    return React.createElement(
      "section",
      {
        key,
        className: "tf-break__station",
        "data-station": key,
        "data-state": stopped ? "stopped" : current === key && !done ? "on" : reached ? "done" : "off",
      },
      React.createElement(
        "header",
        { className: "tf-break__shead" },
        React.createElement("span", { className: "tf-break__snum" }, String(si + 1).padStart(2, "0")),
        React.createElement("b", null, name),
        React.createElement("span", { className: "tf-break__swhat" }, what),
        stopped && React.createElement(StateTag, { state: "held", className: "tf-break__stamp" }, "Stopped here"),
      ),
      React.createElement(
        "div",
        { className: "tf-break__frames" },
        mine.map(([f, i]) =>
          React.createElement(BreakFrame, {
            key: i,
            frame: f,
            n: i + 1,
            clicked: f.kind === "card" ? clicks[f.id] : void 0,
            stale: f.kind === "card" && stale[f.id],
          }),
        ),
        events && React.createElement("div", { className: "tf-break__empty" }, "No event. Nothing moved."),
        done && !reached && !events && React.createElement("div", { className: "tf-break__empty" }, "Not reached."),
      ),
    );
  });
  return React.createElement(
    "div",
    { className: cx("tf tf-break", className) },
    React.createElement(
      "div",
      { className: "tf-break__controls", role: "radiogroup", "aria-label": "Attacks" },
      BREAK_GROUPS.map(([group, title]) =>
        React.createElement(
          "div",
          { key: group, className: "tf-break__group" },
          title && React.createElement("span", { className: "tf-break__gtitle" }, title),
          runs.map((r, i) =>
            r.group === group
              ? React.createElement(
                  "button",
                  {
                    key: r.id,
                    type: "button",
                    role: "radio",
                    "aria-checked": i === ri,
                    "data-break-run": r.id,
                    className: "tf-break__opt",
                    onClick: () => choose(i, !reduced),
                  },
                  r.label,
                )
              : null,
          ),
        ),
      ),
    ),
    React.createElement(
      "div",
      { className: "tf-break__stage" },
      React.createElement(
        "div",
        { className: "tf-break__head" },
        React.createElement("span", { className: "tf-rec", "data-live": playing }, "Recorded"),
        React.createElement("span", { className: "tf-break__attack" }, run.attack),
      ),
      React.createElement(
        "div",
        { className: "tf-break__message" },
        React.createElement("span", null, "Message"),
        React.createElement("q", null, run.message),
      ),
      React.createElement("div", { className: "tf-break__stations" }, stations),
      React.createElement(
        "div",
        { className: "tf-break__verdict", "data-kind": done ? run.verdict.kind : "pending", "aria-live": "polite" },
        done ? run.verdict.text : "Playing…",
      ),
    ),
  );
}

export {
  Logo,
  Icon,
  Button,
  StateTag,
  Confirm,
  Claim,
  FrameMeter,
  InstallCommand,
  Segmented,
  Frame,
  CodeBlock,
  TraceLine,
  StepStream,
  Receipt,
  Notice,
  AnswerBlock,
  InteractionCard,
  Message,
  TurnPlayer,
  BreakIt,
  PipelineRail,
  EffortSettings,
  SpecSheet,
  ContrastTable,
  CrateTable,
  Section,
  Manifesto,
  highlight, withClaims, compileTurn, cx, SCENARIOS, STAGES, PHASES, HERO_TURN,
};
