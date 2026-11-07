import { Link, useLocation } from "react-router-dom"
import { LibrarySearch } from "@/components/search"
import { cn } from "../utils/cn"

export function Nav({ children }) {
  return (
    <nav className="relative z-30 flex items-center justify-between gap-6 mx-10 py-6 border-b">
      <img src="/logo/metal-scar.png" alt="Metal Scar Radio" className="w-36 shrink-0" draggable="false" />
      <LibrarySearch />
      <div className="flex-center shrink-0 gap-7">{children}</div>
    </nav>
  )
}
export function NavButton({ path, title }) {
  const pathname = useLocation().pathname
  return (
    <Link
      to={path}
      className={cn(
        "shrink-0 font-semibold text-base transition",
        pathname === path || pathname.startsWith(`${path}/`) ? "text-primary" : "text-secondary hover:text-primary",
      )}
    >
      {title}
    </Link>
  )
}
