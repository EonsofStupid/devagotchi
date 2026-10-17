import Link from "next/link";
import type { ReactNode } from "react";

export function BridgeMark() {
  return <svg viewBox="0 0 36 36" fill="none" aria-hidden="true"><path d="M9 9 7 4M27 9l2-5M5 17H2m32 0h-3M9 28l-2 4m20-4 2 4" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round"/><rect x="6" y="7" width="24" height="23" rx="10" stroke="currentColor" strokeWidth="1.6"/><path d="M13 17v4m10-4v4m-8 4h6" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round"/></svg>;
}

export function Arrow({ diagonal = false }: { diagonal?: boolean }) {
  return <span aria-hidden="true">{diagonal ? "↗" : "→"}</span>;
}

export function BridgeHeader({ active = "" }: { active?: string }) {
  return <header className="bridge-header"><div className="bridge-container bridge-nav"><Link className="bridge-brand" href="/" aria-label="Devagotchi home"><BridgeMark/><span>devagotchi</span></Link><nav aria-label="Primary"><Link href="/#workspace" aria-current={active === "product" ? "page" : undefined}>Product</Link><Link href="/connect" aria-current={active === "connect" ? "page" : undefined}>Connect</Link><Link href="/architecture" aria-current={active === "architecture" ? "page" : undefined}>Architecture</Link><Link href="/research" aria-current={active === "research" ? "page" : undefined}>Research</Link></nav><Link className="bridge-button bridge-button-small" href="/connect#local">Get the preview <Arrow/></Link></div></header>;
}

export function BridgeFooter() {
  return <footer className="bridge-footer"><div className="bridge-container"><div className="bridge-footer-top"><Link className="bridge-brand" href="/"><BridgeMark/><span>devagotchi</span></Link><p>Your AI, at home in your workspace.</p><a href="/api/v1/capabilities">Public capability API <Arrow diagonal/></a></div><div className="bridge-footer-bottom"><span>© {new Date().getUTCFullYear()} Devagotchi</span><div><Link href="/connect#status">Preview status</Link><a href="/openapi.json">OpenAPI</a><a href="/llms.txt">For agents</a><Link href="/connect#source">Source & license</Link></div><span className="bridge-footer-status"><i/> Linux source preview · October 2026</span></div></div></footer>;
}

export function BridgeShell({ children, active = "" }: { children: ReactNode; active?: string }) {
  return <div className="bridge-site"><a className="bridge-skip" href="#main-content">Skip to content</a><BridgeHeader active={active}/>{children}<BridgeFooter/></div>;
}

export function SectionLabel({ children, number }: { children: ReactNode; number?: string }) {
  return <p className="bridge-eyebrow">{number && <span>{number} /</span>}{children}</p>;
}

export function StatusBadge({ children, planned = false }: { children: ReactNode; planned?: boolean }) {
  return <span className={`bridge-badge${planned ? " bridge-badge-planned" : ""}`}><i/>{children}</span>;
}
