import { openUrl } from "@tauri-apps/plugin-opener"
import packageJson from "#/package.json"
import { useI18n } from "@/utils/i18n"

export function AboutPage() {
  const { t } = useI18n()
  return (
    <div className="flex-center flex-col gap-12 h-full p-8">
      <div className="flex-center flex-col gap-2">
        <img src="/logo/metal-scar.png" alt="Metal Scar Radio" className="w-54" />
        <h1 className="text-xl font-bold">{t("appTitle")}</h1>
        <span className="text-secondary pt-1">
          {t("version")} {packageJson.version}
        </span>
      </div>
      <button
        type="button"
        aria-label={t("project")}
        onClick={() => openUrl("https://github.com/wiseCirno/metal-scar-downloader")}
      >
        <img
          src="/logo/github.svg"
          alt="GitHub"
          className="size-6 hover:opacity-70 cursor-pointer transition"
          draggable="false"
        />
      </button>
      <div className="flex-center flex-col gap-2 text-xs text-secondary text-center">
        <p>{t("copyright")}</p>
        <p>{t("trademarks")}</p>
      </div>
    </div>
  )
}
