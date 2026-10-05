const paths = {
  chevron: "m4 6 4 4 4-4",
  close: "m4 4 8 8m0-8-8 8",
  more: "M3 8h.01M8 8h.01M13 8h.01",
  plus: "M8 3v10M3 8h10",
  minus: "M3 8h10",
  fit: "M6 2H2v4m8-4h4v4M2 10v4h4m8-4v4h-4",
};

/** Decorative icon; the surrounding control supplies its accessible name. */
export function Icon(props: { name: keyof typeof paths; size?: number; class?: string }) {
  return <svg class={`icon ${props.class ?? ""}`} width={props.size ?? 16} height={props.size ?? 16} viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d={paths[props.name]} /></svg>;
}
