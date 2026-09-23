---
name: daily-finance-brief
description: 每天 12:00 抓全球主要指数与汇率,统计并与前一天对比,生成图文简报(HTML + PDF)发到个人空间。
schedule: daily 12:00
skills: [report-html]
egress:
  - query1.finance.yahoo.com
  - api.frankfurter.dev
outputs: [html, pdf]
channel: personal
---

# 全球金融日报

## 1. 抓数据(每个来源取不到就换下一个,都取不到就标注缺失,不要编造)
- 指数:用 Yahoo 的 chart 接口取近 5 个交易日收盘价,例如
  https://query1.finance.yahoo.com/v8/finance/chart/%5EGSPC?range=5d&interval=1d
  依次取 ^GSPC(标普 500)、^IXIC(纳指)、^DJI(道指)、^N225(日经 225)、^HSI(恒指)、000001.SS(上证)。
- 汇率:https://api.frankfurter.dev/v1/latest?base=USD&symbols=CNY,JPY,EUR,GBP
- 只取公开行情,URL 里不带任何账号或个人参数。

## 2. 存数据
把每个标的的最新收盘、前一日收盘、日涨跌幅、时间戳写进 data/<今天>.json;
如果 data/ 里有更早的文件,读最近一份做环比(今天的最新值 vs 上一份报告的最新值)。

## 3. 写报告 report.html(按技能 report-html 的规范)
- 开头一句话总结市场基调(涨多跌多、最大波动在哪)。
- 表格:各指数与汇率的最新值、日涨跌幅、与上一份报告相比的变化。
- 图:各指数日涨跌幅的横向柱状图(内联 SVG);近 5 日走势各画一条小折线。
- 结尾:数据来源与抓取时间;取不到的项明确标"缺失"。

## 4. 写 summary.md
3 到 5 行:基调、涨幅最大、跌幅最大、汇率要点、缺失说明(如有)。
