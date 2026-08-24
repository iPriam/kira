import type { Metadata } from "next";
import Link from "next/link";
import { HomeLayout } from "fumadocs-ui/layouts/home";
import { baseOptions } from "@/lib/layout.shared";

export const metadata: Metadata = {
  title: "Not Found",
};

export default function NotFound() {
  return (
    <HomeLayout {...baseOptions()}>
      <main className="mx-auto flex w-full max-w-3xl flex-1 flex-col items-center justify-center gap-4 px-4 py-20 text-center">
        <span className="kira-badge">404</span>
        <h1 className="kira-display text-4xl font-bold">
          That page is not part of the current Kira docs.
        </h1>
        <p className="max-w-2xl text-lg leading-8 text-fd-muted-foreground">
          The documentation site only publishes the latest English docs right
          now. Use the links below to jump back into the supported sections.
        </p>
        <div className="flex flex-wrap justify-center gap-3">
          <Link className="kira-button kira-button-primary" href="/docs">
            Open Docs
          </Link>
          <Link className="kira-button" href="/">
            Home
          </Link>
        </div>
      </main>
    </HomeLayout>
  );
}
