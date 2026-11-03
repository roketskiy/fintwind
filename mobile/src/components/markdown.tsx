import * as React from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import { HighlightedCode } from "./highlighted-code";
import { MermaidDiagram } from "./mermaid-diagram";

import {
  CodeBlock,
  CodeBlockActions,
  CodeBlockContent,
  CodeBlockCopyButton,
  CodeBlockHeader,
  CodeBlockTitle,
} from "@/components/ui/code-block";

const remarkPlugins = [remarkGfm];
type MathPlugins = typeof import("./math-plugins");
let mathPlugins: MathPlugins | undefined;
let mathLoading: Promise<MathPlugins> | undefined;

function textFromChildren(children: React.ReactNode): string {
  if (typeof children === "string" || typeof children === "number") {
    return String(children);
  }

  if (Array.isArray(children)) {
    return children.map(textFromChildren).join("");
  }

  return "";
}

function safeHref(value: string | undefined): string | null {
  if (!value) {
    return null;
  }

  try {
    const url = new URL(value);
    if (
      url.protocol === "http:" ||
      url.protocol === "https:" ||
      url.protocol === "mailto:"
    ) {
      return url.toString();
    }
  } catch {
    return null;
  }

  return null;
}

function MarkdownCodeBlock({
  code,
  language,
}: {
  code: string;
  language?: string;
}) {
  return (
    <CodeBlock>
      <CodeBlockHeader>
        <CodeBlockTitle>{language || "代码"}</CodeBlockTitle>
        <CodeBlockActions>
          <CodeBlockCopyButton value={code} />
        </CodeBlockActions>
      </CodeBlockHeader>
      <CodeBlockContent><HighlightedCode code={code} language={language} /></CodeBlockContent>
    </CodeBlock>
  );
}

const components: Components = {
  a({ href, children, title }) {
    const safe = safeHref(href);
    if (!safe) {
      return <>{children}</>;
    }

    return (
      <a href={safe} title={title} target="_blank" rel="noreferrer noopener">
        {children}
      </a>
    );
  },
  img({ alt }) {
    return alt ? <span>{alt}</span> : null;
  },
  pre({ children }) {
    const child = React.Children.toArray(children)[0];

    if (
      !React.isValidElement<{ className?: string; children?: React.ReactNode }>(
        child,
      )
    ) {
      const text = textFromChildren(children).replace(/\n$/, "");
      return text ? <MarkdownCodeBlock code={text} /> : null;
    }

    const className = child.props.className || "";
    const language = /(?:^|\s)language-([^\s]+)/.exec(className)?.[1];
    const code = textFromChildren(child.props.children).replace(/\n$/, "");

    return language?.toLowerCase() === "mermaid" ? <MermaidDiagram source={code} /> : <MarkdownCodeBlock code={code} language={language} />;
  },
  code({ node: _node, className, children, ...props }) {
    return (
      <code className={className} {...props}>
        {children}
      </code>
    );
  },
  table({ node: _node, ...props }) {
    return <table {...props} tabIndex={0} aria-label="表格" />;
  },
  span({ node: _node, className, ...props }) {
    return <span {...props} className={className} {...(className?.split(/\s+/).includes("katex-display") ? { tabIndex: 0, "aria-label": "块级公式" } : {})} />;
  },
};

function transformUrl(url: string) {
  return safeHref(url) ?? "";
}

export const Markdown = React.memo(function Markdown({
  text,
}: {
  text: string;
}) {
  const hasMath = /\$|\\(?:\(|\[|begin\{)|(?:```|~~~)math\b/.test(text);
  const [math, setMath] = React.useState(() => mathPlugins);
  React.useEffect(() => {
    if (!hasMath || math) return;
    let active = true;
    mathLoading ??= import("./math-plugins").then(plugins => { mathPlugins = plugins; return plugins; });
    void mathLoading.then(plugins => { if (active) setMath(plugins); }, () => { /* Keep readable plain Markdown if loading fails. */ });
    return () => { active = false; };
  }, [hasMath, math]);
  const content = React.useMemo(() => hasMath && math ? math.normalizeTexMath(text) : text, [text, hasMath, math]);
  return (
    <div className="markdown-body min-w-0">
      <ReactMarkdown
        remarkPlugins={hasMath && math ? math.remarkPlugins : remarkPlugins}
        rehypePlugins={hasMath && math ? math.rehypePlugins : undefined}
        skipHtml
        urlTransform={transformUrl}
        components={components}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
});
