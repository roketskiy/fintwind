import { useEffect, useId, useRef, useState } from "react";
import { CheckIcon, CloseIcon, SearchIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Field, FieldGroup, FieldLabel } from "@/components/ui/field";
import { InputGroup, InputGroupAddon, InputGroupButton, InputGroupInput } from "@/components/ui/input-group";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { Spinner } from "@/components/ui/spinner";
import { errorMessage } from "@/lib/error-message";
import type { ComposerOptions } from "@/lib/types";
import "./model-picker.css";

function providerLabel(provider?: string) {
  const label = provider?.trim();
  return !label || label.toLowerCase() === "opencode" ? "OpenCode" : label;
}

export function ModelPicker({ catalog, selected, disabled, onSelect }: {
  catalog: ComposerOptions;
  selected: string;
  disabled: boolean;
  // null means accepted; failure text is shown inside the still-open picker.
  onSelect: (id: string) => Promise<string | null>;
}) {
  const searchId = useId();
  const [query, setQuery] = useState("");
  const [group, setGroup] = useState<string | null>(null);
  const [pending, setPending] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const selecting = useRef(false);
  const list = useRef<HTMLDivElement>(null);
  const search = useRef<HTMLInputElement>(null);
  const favorites = catalog.favoriteModelIds ?? [];
  const providers = [...new Set(catalog.models.map(model => providerLabel(model.provider)))];
  const selectedModel = catalog.models.find(model => model.id === selected);
  const currentProvider = selectedModel ? providerLabel(selectedModel.provider) : providers[0];
  const activeGroup = group ?? (currentProvider ? `provider:${currentProvider}` : "favorites");
  const tokens = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const searching = tokens.length > 0;
  const visible = catalog.models.filter(model => searching
    ? tokens.every(token => `${model.name} ${model.id} ${providerLabel(model.provider)}`.toLowerCase().includes(token))
    : activeGroup === "favorites" ? favorites.includes(model.id) : `provider:${providerLabel(model.provider)}` === activeGroup);
  if (!searching && activeGroup === "favorites") visible.sort((a, b) => favorites.indexOf(a.id) - favorites.indexOf(b.id));

  useEffect(() => {
    if (!list.current) return;
    list.current.scrollTop = 0;
    if (!query.trim()) list.current.querySelector('[aria-current="true"]')?.scrollIntoView({ block: "nearest" });
  }, [activeGroup, query]);

  async function choose(id: string) {
    if (disabled || selecting.current) return;
    selecting.current = true;
    setPending(id);
    setError(null);
    try { setError(await onSelect(id)); }
    catch (cause) { setError(errorMessage(cause)); }
    finally { selecting.current = false; setPending(null); }
  }

  return <div className="model-picker">
    <FieldGroup className="model-picker-search">
      <Field>
        <FieldLabel className="sr-only" htmlFor={searchId}>搜索模型</FieldLabel>
        <InputGroup>
          <InputGroupInput ref={search} id={searchId} placeholder="搜索模型或提供商" value={query}
            autoComplete="off" autoCapitalize="none" spellCheck={false}
            onChange={event => setQuery(event.target.value)} />
          <InputGroupAddon><SearchIcon /></InputGroupAddon>
          {query && <InputGroupAddon align="inline-end">
            <InputGroupButton size="icon-sm" aria-label="清除搜索" onClick={() => { setQuery(""); search.current?.focus(); }}><CloseIcon /></InputGroupButton>
          </InputGroupAddon>}
        </InputGroup>
      </Field>
    </FieldGroup>
    <div className="model-picker-columns">
      <div className="model-picker-sidebar">
        <ToggleGroup className="model-picker-groups" orientation="vertical" multiple={false} spacing={1}
          aria-label="模型分组" value={searching ? [] : [activeGroup]}
          onValueChange={values => { if (values[0]) { setGroup(String(values[0])); setQuery(""); } }}>
          <ToggleGroupItem value="favorites">收藏</ToggleGroupItem>
          {providers.map(provider => <ToggleGroupItem key={provider} value={`provider:${provider}`} title={provider}>{provider}</ToggleGroupItem>)}
        </ToggleGroup>
      </div>
      <div ref={list} className="model-picker-list" aria-busy={pending !== null || undefined}
        aria-label={searching ? "模型搜索结果" : activeGroup === "favorites" ? "收藏模型" : `${activeGroup.slice(9)} 模型`}>
        {!visible.length && <p className="model-picker-empty" role="status">{!searching && activeGroup === "favorites" ? "暂无收藏模型" : "没有匹配的模型"}</p>}
        {visible.map(model => <Button key={model.id} type="button" variant="row" className="model-picker-row"
          aria-current={model.id === selected ? "true" : undefined} disabled={disabled || pending !== null}
          title={model.name || model.id} onClick={() => void choose(model.id)}>
          <span className="model-picker-name"><strong>{model.name || model.id}</strong>
            {(searching || activeGroup === "favorites") && <small>{providerLabel(model.provider)}</small>}
          </span>
          {pending === model.id ? <Spinner data-icon="inline-end" /> : model.id === selected ? <CheckIcon data-icon="inline-end" /> : null}
        </Button>)}
      </div>
    </div>
    {error && <Alert variant="destructive" className="model-picker-error"><AlertDescription>{error}</AlertDescription></Alert>}
  </div>;
}
