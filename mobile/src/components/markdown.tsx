import * as React from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";

import {
  CodeBlock,
  CodeBlockActions,
  CodeBlockContent,
  CodeBlockCopyButton,
  CodeBlockHeader,
  CodeBlockTitle,
} from "@/components/ui/code-block";

const remarkPlugins = [remarkGfm];

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
      <CodeBlockContent>{code}</CodeBlockContent>
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

    return <MarkdownCodeBlock code={code} language={language} />;
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
};

function transformUrl(url: string) {
  return safeHref(url) ?? "";
}

export const Markdown = React.memo(function Markdown({
  text,
}: {
  text: string;
}) {
  return (
    <div className="markdown-body min-w-0">
      <ReactMarkdown
        remarkPlugins={remarkPlugins}
        skipHtml
        urlTransform={transformUrl}
        components={components}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
});
