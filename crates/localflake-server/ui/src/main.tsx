import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import { createBrowserRouter } from "react-router"
import { RouterProvider } from "react-router/dom"

import "./index.css"
import "./theme.css"
import { ThemeProvider } from "@/components/theme-provider"
import { TooltipProvider } from "@/components/ui/tooltip"
import { startPolling } from "@/lib/live"
import { initWorksheets } from "@/lib/worksheets"
import {
  HistoryDetail,
  HistoryView,
  historyRecordLoader,
} from "@/routes/history"
import { NotFound } from "@/routes/not-found"
import { Loading, Root, RouteError, rootLoader } from "@/routes/root"
import { SessionsView } from "@/routes/sessions"

/** The server serves the app here. */
const BASE = "/__localflake/ui"

// Links from before the app moved to path routes: #history is now /history.
if (location.hash === "#history" || location.hash === "#sessions") {
  history.replaceState(null, "", `${BASE}/${location.hash.slice(1)}`)
}

initWorksheets()
startPolling()

const router = createBrowserRouter(
  [
    {
      id: "root",
      path: "/",
      loader: rootLoader,
      // localflake's version and storage don't change while it runs.
      shouldRevalidate: () => false,
      Component: Root,
      ErrorBoundary: RouteError,
      HydrateFallback: Loading,
      children: [
        // The worksheets: Root keeps them mounted, and shows them here.
        { index: true },
        {
          path: "history",
          Component: HistoryView,
          children: [
            {
              path: ":queryId",
              loader: historyRecordLoader,
              Component: HistoryDetail,
              ErrorBoundary: RouteError,
            },
          ],
        },
        { path: "sessions", Component: SessionsView },
        { path: "*", Component: NotFound },
      ],
    },
  ],
  { basename: BASE }
)

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ThemeProvider storageKey="localflake.theme">
      <TooltipProvider>
        <RouterProvider router={router} />
      </TooltipProvider>
    </ThemeProvider>
  </StrictMode>
)
