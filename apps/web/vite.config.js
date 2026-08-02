// 打成**单个自包含 JS**:内网部署连不上 CDN,依赖必须随包走。
// 产物由 muster-server 用 include_str! 嵌进二进制 —— 第二台机器什么都不用装。
import { defineConfig } from "vite";
export default defineConfig({
  build: {
    lib: { entry: "src/main.js", formats: ["iife"], name: "MusterWeb", fileName: () => "app.js" },
    outDir: "dist",
    minify: "esbuild",
    emptyOutDir: true,
  },
});
