// CodeMirror 6 language support for the leadsheet (.ls) format:
// a StreamLanguage tokenizer plus the playground's highlight/theme styles.
// The tokenizer is presentation-only — leadsheet-wasm `check` is the
// authority on validity.

import {
  EditorView,
  StreamLanguage,
  syntaxHighlighting,
  HighlightStyle,
  tags as t,
} from "./vendor/codemirror.js";

const LANE_LABEL =
  /^(K2?|S2?|st|cp|hp|h|O|C2?|Cs|Cn|R2?|rb|T[1-6]|tm|cb|vs|B[12]|cg[123]|d\d{1,3})(?=\s*\|)/;

const lsStream = {
  name: "leadsheet",

  startState() {
    // mode: what pattern body the cursor is inside ("mel" | "chord" | "drums")
    // arr: inside the arrangement: block
    return { mode: null, arr: false, inBody: false };
  },

  token(stream, state) {
    if (stream.sol()) state.inBody = false;

    if (stream.sol() && stream.match(/^#/)) {
      stream.skipToEnd();
      return "comment";
    }

    if (stream.sol() && stream.match(/^arrangement:/)) {
      state.arr = true;
      state.mode = null;
      return "heading";
    }

    // Arrangement rows: indented `label: [P1+P2] x4`
    if (state.arr && /\s/.test(stream.string[0] ?? "")) {
      if (stream.eatSpace()) return null;
      if (stream.match(/^[A-Za-z][\w-]*:/)) return "heading";
      if (stream.match(/^P\d+/)) return "labelName";
      if (stream.match(/^x\d+/)) return "number";
      if (stream.match(/^z/)) return "null";
      if (stream.match(/^[[\]+]/)) return "bracket";
      stream.next();
      return null;
    }
    if (stream.sol()) state.arr = false;

    // Pattern / direct-bar header: `P7 keys*@mp ~P3 | ... |`
    if (stream.sol() && stream.match(/^(P|b)\d+/)) {
      state.mode = "mel";
      return "labelName";
    }
    if (!state.inBody && !stream.string.trimStart().startsWith("|")) {
      if (stream.eatSpace()) return null;
      if (stream.match(/^~P\d+/)) return "labelName";
      if (stream.match(/^@(pp|ppp|p|mp|mf|ff|f)\b/)) return "modifier";
      const instr = stream.match(/^[A-Za-z][\w-]*(\*)?/);
      if (instr) {
        if (instr[0] === "drums") state.mode = "drums";
        else if (instr[0].endsWith("*")) state.mode = "chord";
        else state.mode = "mel";
        return "variableName";
      }
    }

    // Drum lane label at the start of an indented line
    if (state.mode === "drums" && /^\s/.test(stream.string) && stream.column() <= 4 && !state.inBody) {
      if (stream.eatSpace()) return null;
      if (stream.match(LANE_LABEL)) return "keyword";
    }

    if (stream.eat("|")) {
      state.inBody = true;
      return "bracket";
    }
    if (!state.inBody) {
      stream.next();
      return null;
    }

    // --- inside a pattern body ---
    if (stream.eatSpace()) return null;

    if (state.mode === "drums") {
      if (stream.match(/^[xX]+/)) return "atom";
      if (stream.match(/^o+/)) return "meta";
      if (stream.match(/^[234]+/)) return "number";
      if (stream.match(/^\.+/)) return "punctuation";
      stream.next();
      return null;
    }

    if (state.mode === "chord") {
      if (stream.match(/^z/)) return "null";
      if (stream.match(/^\./)) return "punctuation";
      if (stream.match(/^[A-G][#b]?[\w#+]*(\([0-9]\))?(\/[A-G][#b]?)?/)) return "className";
      stream.next();
      return null;
    }

    // melodic
    if (stream.match(/^z(\d+(\/\d+)?|\/\d+)?/)) return "null";
    if (stream.match(/^[>~]/)) return "modifier";
    if (stream.match(/^[\^_=]{0,2}[A-Ga-g][,']*/)) return "atom";
    if (stream.match(/^(\d+(\/\d+)?|\/\d+)/)) return "number";
    if (stream.match(/^\(\d+/)) return "paren";
    if (stream.eat(")")) return "paren";
    if (stream.match(/^[[\]]/)) return "bracket";
    if (stream.eat("-")) return "modifier";
    if (stream.eat("&")) return "keyword";
    stream.next();
    return null;
  },
};

export const lsLanguage = StreamLanguage.define(lsStream);

export const lsHighlight = syntaxHighlighting(
  HighlightStyle.define([
    { tag: t.comment, color: "#7d8899", fontStyle: "italic" },
    { tag: t.labelName, color: "#d98e4a", fontWeight: "600" },
    { tag: t.variableName, color: "#6fa8dc" },
    { tag: t.className, color: "#d47fa6" },
    { tag: t.atom, color: "#e8e3da" },
    { tag: t.number, color: "#d99a6c" },
    { tag: t.keyword, color: "#7fc97f" },
    { tag: t.heading, color: "#e8e3da", fontWeight: "700" },
    { tag: t.modifier, color: "#e0b06c" },
    { tag: t.meta, color: "#9a86b8" },
    { tag: t.null, color: "#5d6b80" },
    { tag: t.punctuation, color: "#4a566a" },
    { tag: t.bracket, color: "#8b96a6" },
    { tag: t.paren, color: "#7fc97f" },
  ])
);

export const lsTheme = EditorView.theme(
  {
    "&": {
      backgroundColor: "#0d1b2a",
      color: "#e8e3da",
      fontSize: "14px",
      height: "100%",
    },
    ".cm-content": {
      fontFamily: '"SF Mono", "Fira Code", Menlo, Consolas, monospace',
      caretColor: "#d99a6c",
      paddingBottom: "40vh",
    },
    ".cm-cursor": { borderLeftColor: "#d99a6c" },
    "&.cm-focused .cm-selectionBackground, .cm-selectionBackground": {
      backgroundColor: "#2c3a5c88",
    },
    ".cm-activeLine": { backgroundColor: "#13223a80" },
    ".cm-gutters": {
      backgroundColor: "#0d1b2a",
      color: "#4a566a",
      border: "none",
      borderRight: "1px solid #1f2a44",
    },
    ".cm-activeLineGutter": { backgroundColor: "#13223a" },
    ".cm-lintRange-error": {
      backgroundImage: "none",
      textDecoration: "underline wavy #e06c6c",
      textUnderlineOffset: "3px",
    },
    ".cm-tooltip": {
      backgroundColor: "#1f2a44",
      color: "#e8e3da",
      border: "1px solid #2c3a5c",
      fontFamily: '"SF Mono", Menlo, monospace',
      fontSize: "12.5px",
    },
    ".cm-panels": { backgroundColor: "#13223a", color: "#e8e3da" },
  },
  { dark: true }
);
