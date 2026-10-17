import type { Metadata } from "next";
import { headers } from "next/headers";
import { Geist, Geist_Mono } from "next/font/google";
import "./generated-tokens.css";
import "./globals.css";
import "./bridge.css";

const geistSans = Geist({
  variable: "--font-geist-sans",
  subsets: ["latin"],
});

const geistMono = Geist_Mono({
  variable: "--font-geist-mono",
  subsets: ["latin"],
});

export async function generateMetadata(): Promise<Metadata> {
  const requestHeaders = await headers();
  const host = requestHeaders.get("x-forwarded-host") ?? requestHeaders.get("host") ?? "devagotchi.justsayit.chatgpt.site";
  const protocol = requestHeaders.get("x-forwarded-proto") ?? (host.startsWith("localhost") ? "http" : "https");
  const socialImage = `${protocol}://${host}/images/devagotchi-linux-preview.png`;
  const title = "Devagotchi — Your AI, at home in your workspace";
  const description = "A local desktop bridge connecting AI and your workspace. Explore the Linux source preview, public API and MCP reference, and the task graph and internal mesh roadmap.";
  return {
    title,
    description,
    openGraph: { title, description, type: "website", images: [{ url: socialImage, width: 1380, height: 900, alt: "Devagotchi native Linux workspace inspector" }] },
    twitter: { card: "summary_large_image", title, description, images: [socialImage] },
    icons: { icon: "/favicon.svg", shortcut: "/favicon.svg" },
  };
}

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html lang="en">
      <body className={`${geistSans.variable} ${geistMono.variable}`}>
        {children}
      </body>
    </html>
  );
}
