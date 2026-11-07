import { Channel, invoke } from "@tauri-apps/api/core"
import { open } from "@tauri-apps/plugin-dialog"
import { platform } from "@tauri-apps/plugin-os"
import { useEffect, useRef, useState } from "react"
import { Button } from "@/components/button"
import { Input } from "@/components/input"
import { Select } from "@/components/select"
import { SettingsItem } from "@/components/settings"
import { cn } from "@/utils/cn"
import { getConfig, resetConfig, saveConfig } from "@/utils/config"
import { languageOptions, safeNeteaseErrorCode, useI18n } from "@/utils/i18n"
import { useAppStore } from "@/utils/store"

export function SettingsPage() {
  const { t, setLanguage } = useI18n()
  const { reloadLocalMusic, setAllSongStatuses, songList, loading, isDownloading } = useAppStore()
  const [config, setConfig] = useState(null)
  const [status, setStatus] = useState(null)
  const [ffmpegBusy, setFFmpegBusy] = useState(false)
  const [ffmpegProgress, setFFmpegProgress] = useState(null)
  const ffmpegLock = useRef(false)
  const [auth, setAuth] = useState(null)
  const [authBusy, setAuthBusy] = useState(false)
  const authLock = useRef(false)
  const [phone, setPhone] = useState("")
  const [countrycode, setCountrycode] = useState("86")
  const [captcha, setCaptcha] = useState("")
  const [captchaCooldown, setCaptchaCooldown] = useState(0)
  const [isUpdatingDownloadStatus, setIsUpdatingDownloadStatus] = useState(false)
  const [isSavingConfig, setIsSavingConfig] = useState(false)
  const [isResettingConfig, setIsResettingConfig] = useState(false)
  const configActionLock = useRef(false)

  useEffect(() => {
    let active = true
    getConfig()
      .then((loaded) => {
        if (active) setConfig((current) => current ?? loaded)
      })
      .catch(() => {
        if (active) setStatus({ type: "error", message: t("configLoadFailed") })
      })
    return () => {
      active = false
    }
  }, [t])

  useEffect(() => {
    let active = true
    invoke("netease_auth_status")
      .then((result) => {
        if (active) setAuth({ logged_in: result?.logged_in === true, nickname: result?.nickname || "" })
      })
      .catch(() => {
        if (active) {
          setAuth({ logged_in: false, nickname: "" })
          setStatus({ type: "error", message: t("neteaseStatusFailed") })
        }
      })
    return () => {
      active = false
    }
  }, [t])

  useEffect(() => {
    if (captchaCooldown <= 0) return undefined
    const timer = setInterval(() => setCaptchaCooldown((value) => Math.max(0, value - 1)), 1000)
    return () => clearInterval(timer)
  }, [captchaCooldown])

  const actionBusy =
    loading ||
    isDownloading ||
    ffmpegBusy ||
    authBusy ||
    isSavingConfig ||
    isResettingConfig ||
    isUpdatingDownloadStatus
  const withAuthLock = async (operation) => {
    if (actionBusy || authLock.current || ffmpegLock.current || configActionLock.current) return
    authLock.current = true
    setAuthBusy(true)
    try {
      await operation()
    } finally {
      authLock.current = false
      setAuthBusy(false)
    }
  }
  const sendCaptcha = () => {
    if (!phone.trim() || !countrycode.trim() || captchaCooldown > 0 || auth === null) return
    return withAuthLock(async () => {
      try {
        await invoke("netease_send_captcha", { phone: phone.trim(), countrycode: countrycode.trim() })
        setCaptchaCooldown(60)
        setStatus({ type: "default", message: t("neteaseCaptchaSent") })
      } catch (error) {
        const code = safeNeteaseErrorCode(error)
        setStatus({ type: "error", message: t(code || "neteaseCaptchaSendFailed") })
      }
    })
  }
  const login = () => {
    if (!phone.trim() || !countrycode.trim() || !captcha.trim() || auth === null) return
    return withAuthLock(async () => {
      try {
        const result = await invoke("netease_login", {
          phone: phone.trim(),
          countrycode: countrycode.trim(),
          captcha: captcha.trim(),
        })
        if (result?.logged_in !== true) throw new Error("NETEASE_AUTH_FAILED")
        setAuth({ logged_in: true, nickname: result.nickname || "" })
        setCaptcha("")
        setStatus({ type: "default", message: t("neteaseLoginSuccess") })
      } catch (error) {
        const code = safeNeteaseErrorCode(error)
        setStatus({ type: "error", message: t(code || "neteaseLoginFailed") })
      }
    })
  }
  const logout = () =>
    withAuthLock(async () => {
      try {
        await invoke("netease_logout")
        setAuth({ logged_in: false, nickname: "" })
        setPhone("")
        setCaptcha("")
        setStatus({ type: "default", message: t("neteaseLogoutSuccess") })
      } catch {
        setStatus({ type: "error", message: t("neteaseLogoutFailed") })
      }
    })

  const change = (key, value) => setConfig((current) => ({ ...current, [key]: value }))
  const chooseFolder = async () => {
    const selected = await open({ directory: true, multiple: false })
    if (selected) change("download_path", selected)
  }
  const submit = async (event) => {
    event.preventDefault()
    if (actionBusy || configActionLock.current) {
      setStatus({ type: "warning", message: t("saveBusy") })
      return
    }
    configActionLock.current = true
    setIsSavingConfig(true)
    try {
      await saveConfig(config)
      setLanguage(config.language)
      await reloadLocalMusic()
      setStatus({ type: "default", message: t("configChanged") })
    } catch {
      setStatus({ type: "error", message: t("saveFailed") })
    } finally {
      configActionLock.current = false
      setIsSavingConfig(false)
    }
  }
  const setAllDownloadStatus = async (downloaded) => {
    if (actionBusy || !songList?.length) return
    setIsUpdatingDownloadStatus(true)
    try {
      const changed = await invoke("set_all_download_status", { downloaded })
      setAllSongStatuses(downloaded)
      setStatus({
        type: "default",
        message: t("batchStatus")
          .replace("{count}", changed)
          .replace("{state}", downloaded ? t("downloadedMark") : t("notDownloaded")),
      })
    } catch {
      setStatus({ type: "error", message: t("operationFailed") })
    } finally {
      setIsUpdatingDownloadStatus(false)
    }
  }

  const manageFFmpeg = async (action) => {
    if (actionBusy || ffmpegLock.current || configActionLock.current) return
    ffmpegLock.current = true
    setFFmpegBusy(true)
    setFFmpegProgress(null)
    try {
      if (action === "download") {
        const onProgress = new Channel()
        onProgress.onmessage = ({ stage, downloaded, total }) => {
          if (stage === "extracting") setFFmpegProgress(t("ffmpegExtracting"))
          else {
            const size = (downloaded / 1024 / 1024).toFixed(1)
            const limit = total ? ` / ${(total / 1024 / 1024).toFixed(1)} MB` : " MB"
            setFFmpegProgress(`${t("ffmpegDownloading")} ${size}${limit}`)
          }
        }
        await invoke("download_ffmpeg", { onProgress })
      }
      if (action === "delete") {
        await invoke("delete_ffmpeg")
        setStatus({ type: "default", message: t("ffmpegDeleted") })
      } else {
        const version = await invoke("get_ffmpeg_version")
        setStatus({ type: "default", message: version })
      }
    } catch {
      setStatus({ type: "error", message: t("ffmpegFailed") })
    } finally {
      ffmpegLock.current = false
      setFFmpegBusy(false)
      setFFmpegProgress(null)
    }
  }

  const reset = async () => {
    if (actionBusy || configActionLock.current) {
      setStatus({ type: "warning", message: t("saveBusy") })
      return
    }
    configActionLock.current = true
    setIsResettingConfig(true)
    try {
      await resetConfig()
      const nextConfig = await getConfig()
      setConfig(nextConfig)
      setLanguage(nextConfig.language)
      await reloadLocalMusic()
      setStatus({ type: "default", message: t("saved") })
    } catch {
      setStatus({ type: "error", message: t("saveFailed") })
    } finally {
      configActionLock.current = false
      setIsResettingConfig(false)
    }
  }

  if (!config) return <div className="flex-center min-h-full">{t("loading")}</div>
  const boolOptions = [
    { value: false, label: t("disabled") },
    { value: true, label: t("enabled") },
  ]
  const qualityOptions = [
    { value: "hires", label: t("qualityHires") },
    { value: "lossless", label: t("qualityLossless") },
    { value: "exhigh", label: t("qualityExhigh") },
    { value: "standard", label: t("qualityStandard") },
  ]

  return (
    <form onSubmit={submit} className="flex flex-col h-full overflow-hidden">
      <div className="flex-1 p-8 pl-10 overflow-y-scroll">
        <div className="flex flex-col gap-8 max-w-150 mx-auto">
          <SettingsItem title={t("language")}>
            <Select options={languageOptions} value={config.language} onChange={(value) => change("language", value)} />
          </SettingsItem>
          <SettingsItem title={t("neteaseAuthTitle")} desc={t("neteaseAuthHint")}>
            <div className="flex flex-col gap-3 w-full">
              <div className="text-sm" role="status" aria-live="polite">
                {auth === null
                  ? t("loading")
                  : auth.logged_in
                    ? `${t("neteaseLoggedIn")}${auth.nickname ? ` · ${auth.nickname}` : ""}`
                    : t("neteaseLoggedOut")}
              </div>
              {auth !== null &&
                (auth.logged_in ? (
                  <Button type="button" disabled={actionBusy} onClick={logout}>
                    {authBusy ? t("neteaseLoggingOut") : t("neteaseLogout")}
                  </Button>
                ) : (
                  <>
                    <div className="flex gap-2 w-full">
                      <Input
                        aria-label={t("countryCode")}
                        inputMode="numeric"
                        autoComplete="tel-country-code"
                        value={countrycode}
                        onInput={(event) => setCountrycode(event.target.value.replace(/\D/g, "").slice(0, 4))}
                        placeholder="86"
                        className="w-20 flex-none"
                        disabled={actionBusy || auth === null}
                        onKeyDown={(event) => event.key === "Enter" && event.preventDefault()}
                      />
                      <Input
                        aria-label={t("phoneNumber")}
                        type="tel"
                        inputMode="tel"
                        autoComplete="tel-national"
                        value={phone}
                        onInput={(event) => setPhone(event.target.value.replace(/[^\d]/g, "").slice(0, 20))}
                        placeholder={t("phoneNumber")}
                        disabled={actionBusy || auth === null}
                        onKeyDown={(event) => event.key === "Enter" && event.preventDefault()}
                      />
                      <Button
                        type="button"
                        disabled={actionBusy || !phone.trim() || !countrycode.trim() || captchaCooldown > 0}
                        onClick={sendCaptcha}
                      >
                        {authBusy
                          ? t("neteaseSendingCaptcha")
                          : captchaCooldown
                            ? `${t("neteaseResendIn")} ${captchaCooldown}s`
                            : t("neteaseSendCaptcha")}
                      </Button>
                    </div>
                    <div className="flex gap-2 w-full">
                      <Input
                        aria-label={t("smsCode")}
                        inputMode="numeric"
                        autoComplete="one-time-code"
                        value={captcha}
                        onInput={(event) => setCaptcha(event.target.value.replace(/\D/g, "").slice(0, 12))}
                        placeholder={t("smsCode")}
                        disabled={actionBusy || auth === null}
                        onKeyDown={(event) => {
                          if (event.key === "Enter") {
                            event.preventDefault()
                            login()
                          }
                        }}
                      />
                      <Button
                        type="button"
                        disabled={actionBusy || !phone.trim() || !countrycode.trim() || !captcha.trim()}
                        onClick={login}
                      >
                        {authBusy ? t("neteaseLoggingIn") : t("neteaseLogin")}
                      </Button>
                    </div>
                  </>
                ))}
            </div>
          </SettingsItem>
          <SettingsItem title={t("audioQuality")} desc={t("audioQualityHint")}>
            <Select
              options={qualityOptions}
              value={config.audio_quality}
              onChange={(value) => change("audio_quality", value)}
            />
          </SettingsItem>
          <SettingsItem title={t("downloadPath")} desc={t("downloadPathHint")}>
            <Input
              value={config.download_path}
              onInput={(event) => change("download_path", event.target.value)}
              placeholder={t("select")}
            />
            <Button
              type="button"
              onClick={chooseFolder}
              disabled={actionBusy}
              className="border-l-0 border-border hover:border-border"
            >
              {t("choose")}
            </Button>
          </SettingsItem>
          <SettingsItem title={t("concurrency")} desc={t("concurrencyHint")}>
            <Select
              options={[1, 2, 3, 4, 5, 6].map((value) => ({ value, label: String(value) }))}
              value={config.concurrent_downloads}
              onChange={(value) => change("concurrent_downloads", value)}
            />
          </SettingsItem>
          <SettingsItem title={t("instrumental")} desc={t("instrumentalHint")}>
            <Select
              options={boolOptions}
              value={config.download_instrumental}
              onChange={(value) => change("download_instrumental", value)}
            />
          </SettingsItem>
          <SettingsItem title={t("lyrics")} desc={t("lyricsHint")}>
            <Select
              options={boolOptions}
              value={config.download_lyrics}
              onChange={(value) => change("download_lyrics", value)}
            />
          </SettingsItem>
          <SettingsItem title={t("artistSeparator")} desc={t("artistSeparatorHint")}>
            <Select
              options={[
                { value: ";", label: t("separatorSemicolon") },
                { value: "/", label: t("separatorSlash") },
                { value: ",", label: t("separatorComma") },
                { value: "、", label: t("separatorIdeographicComma") },
                { value: " & ", label: t("separatorAmpersand") },
              ]}
              value={config.artist_separator}
              onChange={(value) => change("artist_separator", value)}
            />
          </SettingsItem>
          <SettingsItem title={t("dateMetadata")} desc={t("dateMetadataHint")}>
            <Select
              options={[
                { value: "year", label: t("dateYear") },
                { value: "full", label: t("dateFull") },
              ]}
              value={config.id3_date_format}
              onChange={(value) => change("id3_date_format", value)}
            />
          </SettingsItem>
          <SettingsItem title={t("albumFolder")} desc={t("albumFolderHint")}>
            <Select
              options={[
                { value: "none", label: t("albumNone") },
                { value: "year", label: t("albumYear") },
                { value: "lite", label: t("albumLite") },
                { value: "full", label: t("albumFull") },
              ]}
              value={config.custom_album}
              onChange={(value) => change("custom_album", value)}
            />
          </SettingsItem>
          <SettingsItem title={t("cover")} desc={t("coverHint")}>
            <Select options={boolOptions} value={config.show_cover} onChange={(value) => change("show_cover", value)} />
          </SettingsItem>
          <SettingsItem title={t("ffmpegTitle")} desc={t("ffmpegHint")}>
            <div className="flex flex-col gap-2 w-full">
              <div className="flex gap-2 flex-wrap">
                {platform() === "windows" && (
                  <>
                    <Button type="button" disabled={actionBusy} onClick={() => manageFFmpeg("download")}>
                      {t("ffmpegInstall")}
                    </Button>
                    <Button type="button" disabled={actionBusy} onClick={() => manageFFmpeg("delete")}>
                      {t("ffmpegDelete")}
                    </Button>
                  </>
                )}
                <Button type="button" disabled={actionBusy} onClick={() => manageFFmpeg("check")}>
                  {t("ffmpegVersion")}
                </Button>
              </div>
              {platform() !== "windows" && (
                <span className="text-xs text-secondary">
                  {platform() === "macos" ? t("ffmpegMac") : t("ffmpegLinux")}
                </span>
              )}
              {ffmpegProgress && (
                <span role="status" className="text-sm">
                  {ffmpegProgress}
                </span>
              )}
            </div>
          </SettingsItem>
          <SettingsItem title={t("downloadStatus")} desc={t("downloadStatusHint")} className="gap-2">
            <Button type="button" onClick={() => setAllDownloadStatus(true)} disabled={actionBusy || !songList?.length}>
              {t("markAllDownloaded")}
            </Button>
            <Button
              type="button"
              onClick={() => setAllDownloadStatus(false)}
              disabled={actionBusy || !songList?.length}
            >
              {t("markAllPending")}
            </Button>
          </SettingsItem>
        </div>
      </div>
      <div className="flex-center justify-end gap-4 mx-10 py-6 border-t">
        {status && (
          <span
            className={cn(
              "flex-1",
              status.type === "error" && "text-red-400",
              status.type === "warning" && "text-amber-400",
            )}
          >
            {status.message}
          </span>
        )}
        <Button type="button" onClick={reset} disabled={actionBusy}>
          {isResettingConfig ? t("resetting") : t("reset")}
        </Button>
        <Button type="submit" disabled={actionBusy}>
          {isSavingConfig ? t("saving") : t("save")}
        </Button>
      </div>
    </form>
  )
}
