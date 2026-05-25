// Typed-tag classifier ported verbatim from the v2.3 mockup.
// Splits `ns:value` tags into known buckets, with everything else falling
// through as `generic`. Used by the detail pane and tree badges.

export interface ClassifiedTags {
  commit: string | null;
  plan: string | null;
  critique: string | null;
  harness: string | null;
  kind: string | null;
  generic: string[];
}

export function classifyTags(tags: string[] | undefined): ClassifiedTags {
  const out: ClassifiedTags = {
    commit: null,
    plan: null,
    critique: null,
    harness: null,
    kind: null,
    generic: [],
  };
  for (const tag of tags || []) {
    const [ns, ...rest] = tag.split(':');
    const val = rest.join(':');
    if (ns === 'commit' && val) out.commit = val;
    else if (ns === 'plan' && val) out.plan = val;
    else if (ns === 'critique' && val) out.critique = val;
    else if (ns === 'harness' && val) out.harness = val;
    else if (ns === 'kind' && val) out.kind = val;
    else out.generic.push(tag);
  }
  return out;
}
