import "@/globals.css"
import { listen } from "@tauri-apps/api/event"
import { StrictMode, useEffect } from "react"
import { createRoot } from "react-dom/client"
import { HashRouter, Route, Routes } from "react-router-dom"
import { DownloadPanel } from "@/components/downloads"
import { Nav, NavButton } from "@/components/nav"
import { AppWindow, MainContent } from "@/components/window"
import { AboutPage } from "@/pages/about"
import { HomePage } from "@/pages/home"
import { SettingsPage } from "@/pages/settings"
import { initConfig } from "@/utils/config"
import { I18nProvider, useI18n } from "@/utils/i18n"
import { useAppStore } from "@/utils/store"

async function bootstrap() {
  try {
    await initConfig()
    await useAppStore.getState().initAppData()
  } catch (error) {
    console.error(error)
  }
}
bootstrap()

function App() {
  const { t } = useI18n()
  useEffect(() => {
    let unlistenNavigate
    let unlistenMenuAction
    listen("navigate", (event) => {
      window.location.hash = event.payload
    }).then((unlisten) => {
      unlistenNavigate = unlisten
    })
    listen("menu-action", (event) => {
      const { refreshSongList, downloadSongs } = useAppStore.getState()
      if (event.payload === "refresh") refreshSongList()
      else if (event.payload === "download_all") downloadSongs()
    }).then((unlisten) => {
      unlistenMenuAction = unlisten
    })
    return () => {
      unlistenNavigate?.()
      unlistenMenuAction?.()
    }
  }, [])
  return (
    <AppWindow>
      <Nav>
        <NavButton path="/" title={t("home")} />
        <NavButton path="/settings" title={t("settings")} />
        <NavButton path="/about" title={t("about")} />
      </Nav>
      <MainContent>
        <Routes>
          <Route path="/" element={<HomePage />} />
          <Route path="/settings" element={<SettingsPage />} />
          <Route path="/about" element={<AboutPage />} />
        </Routes>
      </MainContent>
      <DownloadPanel />
    </AppWindow>
  )
}
createRoot(document.getElementById("root")).render(
  <StrictMode>
    <I18nProvider>
      <HashRouter>
        <App />
      </HashRouter>
    </I18nProvider>
  </StrictMode>,
)
