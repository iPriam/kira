import React from "react";
import type { Metadata } from "next";
import { RootProvider } from "fumadocs-ui/provider/next";
import SearchDialog from "@/components/search";
import { siteDescription } from "@/lib/shared";
import "./app.css";

export const metadata: Metadata = {
  title: "Kira Documentation",
  description: siteDescription,
  icons: {
    icon: "/KiraNameIcon.png",
    apple: "/KiraNameIcon.png",
  },
  openGraph: {
    title: "Kira Documentation",
    description: siteDescription,
    type: "website",
  },
  metadataBase: new URL(process.env.NEXT_PUBLIC_URL || "http://localhost:3000"),
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en" suppressHydrationWarning>
      <head>
        <meta charSet="utf-8" />
        <meta name="viewport" content="width=device-width, initial-scale=1" />
        <meta name="theme-color" content="#b76418" />
        <link rel="preconnect" href="https://fonts.googleapis.com" />
        <link
          rel="preconnect"
          href="https://fonts.gstatic.com"
          crossOrigin="anonymous"
        />
        <link
          rel="stylesheet"
          href="https://fonts.googleapis.com/css2?family=IBM+Plex+Mono:wght@400;500;600&family=IBM+Plex+Sans:wght@400;500;600;700&family=Space+Grotesk:wght@500;700&display=swap"
        />
      </head>
      <body className="flex min-h-screen flex-col kira-shell">
        <RootProvider search={{ SearchDialog }}>
          {children}
        </RootProvider>
      </body>
    </html>
  );
}
