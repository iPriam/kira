import type { BaseLayoutProps } from "fumadocs-ui/layouts/shared";
import { KiraWordmark } from "@/components/brand";

export function baseOptions(): BaseLayoutProps {
  return {
    nav: {
      title: (
        <span className="kira-nav-title">
          <KiraWordmark className="h-9 md:h-10" />
        </span>
      ),
    },
    themeSwitch: {
      enabled: false,
    },
  };
}
