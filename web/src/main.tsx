import { createRoot } from "react-dom/client";
import { App } from "./App.tsx";
import "@fontsource-variable/jetbrains-mono"; // terminal panes (self-hosted)
import "./styles.css";

createRoot(document.getElementById("root")!).render(<App />);
