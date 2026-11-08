// Build-time guards on what the app's bundle ships. Each fails the build with
// a message that names the file and the fix, rather than ship a blank icon or
// an unstyled component.
import type { Plugin } from "vite";

// An AST node as the bundler's parser returns it (ESTree, with offsets).
type Node = { type: string; start: number; end: number; [key: string]: unknown };

function isNode(value: unknown): value is Node {
  return typeof value === "object" && value !== null && typeof (value as Node).type === "string";
}

// Calls visit on every node under root, with its parent.
function walk(root: Node, visit: (node: Node, parent: Node | null) => void) {
  const stack: [Node, Node | null][] = [[root, null]];
  while (stack.length > 0) {
    const [node, parent] = stack.pop()!;
    visit(node, parent);
    for (const value of Object.values(node)) {
      if (Array.isArray(value)) {
        for (const item of value) if (isNode(item)) stack.push([item, node]);
      } else if (isNode(value)) {
        stack.push([value, node]);
      }
    }
  }
}

function lineOf(code: string, offset: number) {
  let line = 1;
  for (let i = 0; i < offset && i < code.length; i++) if (code.charCodeAt(i) === 10) line++;
  return line;
}

// What a module id is called in a message: its path in app/ or in its package.
function shortId(root: string, id: string) {
  const pkg = id.lastIndexOf("/node_modules/");
  if (pkg >= 0) return id.slice(pkg + "/node_modules/".length);
  return id.startsWith(`${root}/`) ? id.slice(root.length + 1) : id;
}

// -- Phosphor weights (#123) ------------------------------------------------
//
// Each @phosphor-icons/react 2.1.10 icon imports dist/defs/NAME.es.js, a Map
// of its drawing in all six weights, and picks one when it renders, from its
// weight prop or IconContext. The bundler cannot know which it will never
// draw, so all six ship. phosphorWeights keeps the weights in KEPT, the only
// ones the app's source and the Kumo code it bundles use, and fails the build
// when they use another: that icon would render blank. Upstream:
// phosphor-icons/react#148, "Treeshake unused weights".

const WEIGHTS = ["bold", "duotone", "fill", "light", "regular", "thin"];
const KEPT = new Set(["regular", "bold", "fill"]);
const PHOSPHOR = "@phosphor-icons/react";
const DEFS = /\/@phosphor-icons\/react\/dist\/defs\/[^/]+\.es\.js$/;
// A defs module as 2.1.10 writes it: `new Map([` then six entries, each
// `[\n    "weight",\n ... \n  ]`, joined by commas, then `\n]);`.
const MAP = "new Map([";
const ENTRY = /\n  \[\n    "([a-z]+)",\n[\s\S]*?\n  \]/g;
// Phosphor's modules the transform knows: the index, one wrapper per icon,
// IconBase and its context, and the defs it rewrites.
const PHOSPHOR_KNOWN = /\/@phosphor-icons\/react\/dist\/(index\.es\.js|csr\/[^/]+\.es\.js|lib\/[^/]+\.es\.js|defs\/[^/]+\.es\.js)$/;

function stringValue(node: Node): string | null {
  if (node.type === "Literal" && typeof node.value === "string") return node.value;
  if (node.type === "TemplateLiteral" && (node.expressions as Node[]).length === 0) {
    const quasi = (node.quasis as Node[])[0] as { value: { cooked: string | null } } | undefined;
    return quasi?.value.cooked ?? null;
  }
  return null;
}

function unwrap(node: Node): Node {
  let n = node;
  while (
    n.type === "TSAsExpression" ||
    n.type === "TSSatisfiesExpression" ||
    n.type === "TSNonNullExpression" ||
    n.type === "TSTypeAssertion" ||
    n.type === "ParenthesizedExpression" ||
    n.type === "JSXExpressionContainer"
  ) {
    n = n.expression as Node;
  }
  return n;
}

// The weights an expression can take, or null when the check cannot tell.
// It reads a string, and either branch of a ? b : c.
function weightsOf(node: Node): string[] | null {
  const n = unwrap(node);
  const text = stringValue(n);
  if (text !== null) return [text];
  if (n.type === "ConditionalExpression") {
    const a = weightsOf(n.consequent as Node);
    const b = weightsOf(n.alternate as Node);
    return a && b ? [...a, ...b] : null;
  }
  return null;
}

function keyName(node: Node): string | null {
  const key = node.key as Node;
  if (!node.computed && (key.type === "Identifier" || key.type === "JSXIdentifier")) return key.name as string;
  return stringValue(key);
}

// Every weight the module passes, as a list of problems: a weight outside
// KEPT, a weight the check cannot read, and an IconContext value it cannot
// read. A weight is a weight prop or attribute anywhere in the module, the
// same as a key `weight` in IconContext's value.
function weightProblems(code: string, program: Node): { line: number; text: string }[] {
  const problems: { line: number; text: string }[] = [];
  const add = (node: Node, text: string) => problems.push({ line: lineOf(code, node.start), text });
  const check = (value: Node | null, at: Node) => {
    if (value === null) return add(at, "a weight with no value");
    const weights = weightsOf(value);
    if (weights === null) return add(at, "a weight the check cannot read: write it as a string, or a ? b : c of strings");
    for (const w of weights) if (!KEPT.has(w)) add(at, `weight "${w}"`);
  };

  // Local names of Phosphor's IconContext, and of namespaces that hold it.
  const contexts = new Set<string>();
  const namespaces = new Set<string>();
  for (const statement of program.body as Node[]) {
    const source = (statement.source as Node | null | undefined)?.value;
    if (typeof source !== "string" || !source.startsWith(PHOSPHOR)) continue;
    if (statement.type === "ImportDeclaration") {
      for (const s of statement.specifiers as Node[]) {
        if (s.type === "ImportNamespaceSpecifier") namespaces.add((s.local as Node).name as string);
        if (s.type === "ImportSpecifier" && keyName({ ...s, key: s.imported, computed: false }) === "IconContext") {
          contexts.add((s.local as Node).name as string);
        }
      }
    } else if (
      statement.type === "ExportAllDeclaration" ||
      (statement.type === "ExportNamedDeclaration" &&
        (statement.specifiers as Node[]).some((s) => keyName({ ...s, key: s.local, computed: false }) === "IconContext"))
    ) {
      add(statement, "re-exports Phosphor's IconContext, so the check cannot follow it");
    }
  }
  // Whether node names IconContext: Ctx, ns.IconContext, or either's .Provider.
  const isContext = (node: Node): boolean => {
    const n = unwrap(node);
    if ((n.type === "Identifier" || n.type === "JSXIdentifier") && contexts.has(n.name as string)) return true;
    if (n.type === "MemberExpression" || n.type === "JSXMemberExpression") {
      const object = n.object as Node;
      const property = n.property as Node;
      const name = property.name as string;
      if (name === "IconContext" && (object.type === "Identifier" || object.type === "JSXIdentifier")) {
        return namespaces.has(object.name as string);
      }
      return name === "Provider" && isContext(object);
    }
    return false;
  };
  // IconContext's value must be an object written in place, so the weight
  // check reads its weight key.
  const checkValue = (value: Node | null | undefined, at: Node) => {
    if (!value || unwrap(value).type !== "ObjectExpression") {
      add(at, "an IconContext value the check cannot read: write it as an object in place");
    }
  };
  const allowed = new Set<Node>();

  walk(program, (node, parent) => {
    if (node.type === "ObjectExpression") {
      for (const p of node.properties as Node[]) {
        if (p.type === "Property" && keyName(p) === "weight") check(p.value as Node, p);
      }
    } else if (node.type === "JSXAttribute") {
      const name = node.name as Node;
      if (name.type === "JSXIdentifier" && name.name === "weight") check(node.value as Node | null, node);
    } else if (node.type === "JSXOpeningElement" && isContext(node.name as Node)) {
      allowed.add(node.name as Node);
      const value = (node.attributes as Node[]).find(
        (a) => a.type === "JSXAttribute" && (a.name as Node).name === "value",
      );
      checkValue(value?.value as Node | undefined, node);
    } else if (node.type === "CallExpression") {
      // jsx(Ctx.Provider, { value }), createElement(Ctx, { value }), useContext(Ctx).
      const [first, props] = node.arguments as Node[];
      if (first && isContext(first)) {
        allowed.add(first);
        const callee = unwrap(node.callee as Node);
        const fn = (callee.type === "MemberExpression" ? (callee.property as Node).name : callee.name) as string;
        if (fn !== "useContext" && fn !== "use") {
          const value =
            props && props.type === "ObjectExpression"
              ? (props.properties as Node[]).find((p) => p.type === "Property" && keyName(p) === "value")
              : undefined;
          checkValue(value?.value as Node | undefined, node);
        }
      }
    }
    // Any other use of IconContext, such as passing it on, the check cannot follow.
    if (allowed.has(node) || !isContext(node)) return;
    if (parent && (parent.type === "ImportSpecifier" || parent.type === "JSXClosingElement")) return;
    if (parent && (parent.type === "MemberExpression" || parent.type === "JSXMemberExpression")) {
      if (allowed.has(parent) || (parent.object === node && isContext(parent))) return;
    }
    add(node, "IconContext used in a way the check cannot follow");
  });
  return problems.toSorted((a, b) => a.line - b.line);
}

export function phosphorWeights(): Plugin[] {
  let root = "";
  const rewritten = new Set<string>();
  const problems = new Map<string, { line: number; text: string }[]>();
  return [
    {
      name: "residue:phosphor-weights",
      apply: "build",
      configResolved(config) {
        root = config.root;
      },
      transform: {
        filter: { id: DEFS },
        handler(code, id) {
          // Read as text, not parsed: all 1,512 load, and parsing them cost
          // the build about 90 ms and 50 MB. Any other shape fails.
          const open = code.indexOf(MAP);
          const close = code.indexOf("\n]);", open);
          const body = open >= 0 && close >= 0 ? code.slice(open + MAP.length, close) : "";
          const entries = [...body.matchAll(ENTRY)];
          const names = entries.map((m) => m[1]!);
          if (
            code.indexOf(MAP, open + 1) >= 0 ||
            entries.map((m) => m[0]).join(",") !== body ||
            names.toSorted().join() !== WEIGHTS.join()
          ) {
            this.error(
              `${shortId(root, id)} is not the six-weight Map this transform reads. ` +
                "Phosphor's layout changed: check dist/defs, then update phosphorWeights in app/bundle.ts.",
            );
          }
          const kept = entries.filter((m) => KEPT.has(m[1]!)).map((m) => m[0]);
          rewritten.add(id);
          return { code: code.slice(0, open + MAP.length) + kept.join(",") + code.slice(close), map: null };
        },
      },
      generateBundle(_, bundle) {
        for (const chunk of Object.values(bundle)) {
          if (chunk.type !== "chunk") continue;
          for (const id of chunk.moduleIds) {
            if (!id.includes(`/${PHOSPHOR}/`)) continue;
            if (!PHOSPHOR_KNOWN.test(id) || (DEFS.test(id) && !rewritten.has(id))) {
              this.error(
                `${shortId(root, id)} is a Phosphor module the weights transform does not know. ` +
                  "Phosphor's layout changed: check it, then update phosphorWeights in app/bundle.ts.",
              );
            }
          }
        }
      },
    },
    {
      // Runs before the JSX and TypeScript transform, so its lines are the
      // source's. Kumo's code is checked too: its checkbox draws bold icons.
      name: "residue:phosphor-weight-uses",
      apply: "build",
      enforce: "pre",
      transform: {
        filter: {
          id: { include: /\.[cm]?[jt]sx?$/, exclude: /\/@phosphor-icons\/react\// },
          moduleType: ["js", "jsx", "ts", "tsx"],
          code: /\bweight\b|IconContext/,
        },
        handler(code, id) {
          const path = id.split("?")[0]!;
          const inScope = !path.includes("/node_modules/") || path.includes("/@cloudflare/kumo/") || code.includes(PHOSPHOR);
          if (!inScope) return null;
          const ext = path.slice(path.lastIndexOf(".") + 1);
          const lang = ext === "tsx" || ext === "ts" || ext === "jsx" ? ext : "js";
          const found = weightProblems(code, this.parse(code, { lang }) as unknown as Node);
          if (found.length > 0) problems.set(id, found);
          else problems.delete(id);
          return null;
        },
      },
      // Only modules that reach the bundle count: Kumo's index imports every
      // component, and the ones the app does not draw are dropped.
      generateBundle(_, bundle) {
        const lines: string[] = [];
        for (const chunk of Object.values(bundle)) {
          if (chunk.type !== "chunk") continue;
          for (const id of chunk.moduleIds) {
            if ((chunk.modules[id]?.renderedLength ?? 0) === 0) continue;
            for (const p of problems.get(id) ?? []) lines.push(`  ${shortId(root, id)}:${p.line}: ${p.text}`);
          }
        }
        if (lines.length > 0) {
          this.error(
            [
              `The build keeps only Phosphor's ${[...KEPT].join(", ").replace(/, (\w+)$/, " and $1")} weights, so these icons would render blank:`,
              ...lines,
              "Use one of those weights, or add it to KEPT in app/bundle.ts.",
            ].join("\n"),
          );
        }
      },
    },
  ];
}

// -- Kumo chunks in the stylesheet's scan (#124) ----------------------------
//
// src/styles.css scans only src/ and the chunks of the Kumo components the
// app draws, with `@import "tailwindcss" source(none)`, so a component
// missing from its @source list ships without its rules and renders
// unstyled. kumoSources fails the build when the bundle holds a Kumo
// component chunk that list does not name, and when the list names a
// component the bundle no longer holds: its rules are CSS for nothing, and a
// warning on a passing build is read by no one.

const KUMO = "/node_modules/@cloudflare/kumo/";
const COMPONENT = /\/\/#region src\/components\//;

// A chunk's component, from its name: dist/chunks/layer-card-2kx9.js is layer-card.
function component(file: string) {
  return file.slice(file.lastIndexOf("/") + 1).replace(/-[a-z0-9]+\.js$/, "");
}

// A glob from @source as a regular expression: *, ** and ?. Braces are
// expanded before.
function globRegExp(glob: string) {
  let out = "";
  for (let i = 0; i < glob.length; i++) {
    const c = glob[i]!;
    if (c === "*" && glob[i + 1] === "*") {
      out += glob[i + 2] === "/" ? "(?:.*/)?" : ".*";
      i += glob[i + 2] === "/" ? 2 : 1;
    } else if (c === "*") out += "[^/]*";
    else if (c === "?") out += "[^/]";
    else out += c.replace(/[.+^${}()|[\]\\]/g, "\\$&");
  }
  return new RegExp(`^${out}$`);
}

// {a,b}-*.js as a-*.js and b-*.js, one group at a time.
function expandBraces(glob: string): string[] {
  const open = glob.indexOf("{");
  if (open < 0) return [glob];
  const close = glob.indexOf("}", open);
  if (close < 0 || glob.slice(open + 1, close).includes("{")) throw new Error(`cannot read the @source glob ${glob}`);
  return glob
    .slice(open + 1, close)
    .split(",")
    .flatMap((part) => expandBraces(glob.slice(0, open) + part + glob.slice(close + 1)));
}

// The stylesheet's @source lines that name Kumo's files, as globs inside the
// package (dist/chunks/button-*.js), each brace alternative on its own.
function kumoGlobs(css: string) {
  const include: { glob: string; test: RegExp }[] = [];
  const exclude: RegExp[] = [];
  const text = css.replace(/\/\*[\s\S]*?\*\//g, "");
  for (const m of text.matchAll(/@source\s+(not\s+)?(["'])(.*?)\2\s*;/g)) {
    const path = m[3]!;
    const at = path.indexOf(KUMO.slice(1));
    if (at < 0) continue;
    let inside = path.slice(at + KUMO.length - 1);
    if (inside === "" || !/[*?{}]|\.[a-z]+$/.test(inside.split("/").pop()!)) inside = `${inside.replace(/\/$/, "")}/**`;
    for (const glob of expandBraces(inside.replace(/^\/+/, ""))) {
      if (m[1]) exclude.push(globRegExp(glob));
      else include.push({ glob, test: globRegExp(glob) });
    }
  }
  return { include, exclude };
}

export function kumoSources(stylesheet = "src/styles.css"): Plugin {
  let root = "";
  const components = new Set<string>();
  return {
    name: "residue:kumo-sources",
    apply: "build",
    configResolved(config) {
      root = config.root;
    },
    transform: {
      filter: { id: /\/node_modules\/@cloudflare\/kumo\/dist\//, code: COMPONENT },
      handler(_, id) {
        components.add(id);
        return null;
      },
    },
    async generateBundle(_, bundle) {
      const bundled = new Map<string, string>();
      for (const chunk of Object.values(bundle)) {
        if (chunk.type !== "chunk") continue;
        for (const id of chunk.moduleIds) {
          if (components.has(id) && (chunk.modules[id]?.renderedLength ?? 0) > 0) {
            bundled.set(id, id.slice(id.lastIndexOf(KUMO) + KUMO.length));
          }
        }
      }
      const { include, exclude } = kumoGlobs(await this.fs.readFile(`${root}/${stylesheet}`, { encoding: "utf8" }));
      const named = (file: string) => include.some((g) => g.test.test(file)) && !exclude.some((g) => g.test(file));
      const missing = [...bundled.values()].filter((file) => !named(file));
      const stale = include
        .filter((g) => ![...bundled.values()].some((file) => g.test.test(file) && named(file)))
        .map((g) => g.glob);
      const lines: string[] = [];
      if (missing.length > 0) {
        lines.push(
          `The bundle draws Kumo components that ${stylesheet} does not scan, so they would render unstyled:`,
          ...missing.map((file) => `  ${component(file)} (${file})`),
          "Add each name to the Kumo chunks @source line there.",
        );
      }
      if (stale.length > 0) {
        lines.push(
          `${stylesheet} scans Kumo files the bundle does not hold, which adds CSS for nothing:`,
          ...stale.map((glob) => `  ${glob}`),
          "Remove each from the Kumo chunks @source line there.",
        );
      }
      if (lines.length > 0) this.error(lines.join("\n"));
    },
  };
}
