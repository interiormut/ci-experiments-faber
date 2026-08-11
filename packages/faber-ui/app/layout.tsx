import type { Metadata } from "next";
import { Geist_Mono } from "next/font/google";
import "./globals.css";

import { AmbientBackground } from "@/components/ui/ambient-background";
import { AppAuth } from "@/components/shell/app-auth";
import { AppShell } from "@/components/shell/app-shell";

const geistMono = Geist_Mono({
  variable: "--font-geist-mono",
  subsets: ["latin"],
});

export const metadata: Metadata = {
  title: "faber",
  description: "faber UI",
  icons: {
    icon: {
      url: "https://ui.registry.panit.dev/assets/faber-logo-28.png",
      type: "image/png",
      sizes: "28x28",
    },
  },
};

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html
      lang="en"
      className={`${geistMono.variable} h-full antialiased`}
    >
      <head>
        <link
          rel="preconnect"
          href="https://cdn.jsdelivr.net"
          crossOrigin="anonymous"
        />
        <link
          rel="stylesheet"
          href="https://cdn.jsdelivr.net/gh/orioncactus/pretendard@v1.3.9/dist/web/variable/pretendardvariable-dynamic-subset.min.css"
        />
      </head>
      <body className="h-full flex flex-col overflow-hidden">
        <AmbientBackground />
        <AppAuth>
          <AppShell>{children}</AppShell>
        </AppAuth>
      </body>
    </html>
  );
}
