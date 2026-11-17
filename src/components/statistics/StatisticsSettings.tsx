import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../ui/Button";
import { useUsageStatistics } from "./useUsageStatistics";
import { formatPointDate } from "./StatisticsChart";
import { MonoRoundedLineChart } from "./MonoRoundedLineChart";
import { MonoActivityHeatmap } from "./MonoActivityHeatmap";
import {
  aggregateStatistics,
  formatAudioDuration,
  getAvailableYears,
  getStatisticsDateBounds,
  type StatisticsPeriod,
} from "./usageStatistics";

function localToday() {
  const date = new Date();
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
}

export function StatisticsSettings() {
  const { t, i18n } = useTranslation();
  const { data, loading, error, retry } = useUsageStatistics();
  const today = localToday();
  const [period, setPeriod] = useState<StatisticsPeriod>("day");
  const [metric, setMetric] = useState<"words" | "audio">("words");
  const [month, setMonth] = useState(today.slice(0, 7));
  const [year, setYear] = useState(Number(today.slice(0, 4)));
  const [heatmapYear, setHeatmapYear] = useState(Number(today.slice(0, 4)));
  const [windowIndex, setWindowIndex] = useState(
    Math.min(
      2,
      Math.floor(
        (Math.floor(
          (Date.parse(today + "T00:00:00Z") -
            Date.parse(today.slice(0, 4) + "-01-01T00:00:00Z")) /
            86400000,
        ) +
          new Date(today.slice(0, 4) + "-01-01T00:00:00Z").getUTCDay()) /
          140,
      ),
    ),
  );
  const bounds = useMemo(
    () =>
      data
        ? getStatisticsDateBounds(data, today)
        : { firstDay: today, lastDay: today },
    [data, today],
  );
  const firstMonth = bounds.firstDay.slice(0, 7);
  const lastMonth = bounds.lastDay.slice(0, 7);
  const selectedMonth =
    month < firstMonth ? firstMonth : month > lastMonth ? lastMonth : month;
  const [dayYear, dayMonth] = selectedMonth.split("-").map(Number);
  const years = useMemo(
    () => (data ? getAvailableYears(data, today) : []),
    [data, today],
  );
  const selectedYear = years.includes(year) ? year : years[years.length - 1];
  const points = useMemo(
    () =>
      data
        ? aggregateStatistics(
            data,
            period,
            period === "day" ? dayYear : selectedYear,
            dayMonth,
            today,
          )
        : [],
    [data, period, dayYear, selectedYear, dayMonth, today],
  );

  return (
    <div className="mx-auto w-full max-w-[720px] space-y-5">
      <header className="px-1">
        <h1 className="pf-title">{t("statistics.title")}</h1>
      </header>
      {loading && (
        <p role="status" className="text-sm text-text-muted">
          {t("statistics.loading")}
        </p>
      )}
      {error && (
        <div role="alert" className="pf-card px-4 py-3 text-sm">
          <p>{t("statistics.error")}</p>
          <Button
            type="button"
            variant="secondary"
            size="sm"
            onClick={retry}
            className="mt-2"
          >
            {t("plotFlow.retry")}
          </Button>
        </div>
      )}
      {data && (
        <>
          <section aria-label={t("statistics.totals")} className="pf-card">
            <dl className="grid grid-cols-3">
              <div className="px-5 py-4">
                <dt className="pf-label">{t("statistics.totalWords")}</dt>
                <dd className="mt-1 text-[24px] leading-none font-semibold tracking-[-0.03em] tabular-nums">
                  {data.total_words.toLocaleString(i18n.language)}
                </dd>
              </div>
              <div className="border-s border-text/[0.07] px-5 py-4">
                <dt className="pf-label">{t("statistics.totalRecords")}</dt>
                <dd
                  data-statistic="records"
                  className="mt-1 text-[24px] leading-none font-semibold tracking-[-0.03em] tabular-nums"
                >
                  {data.records_complete === true
                    ? (data.total_records ?? 0).toLocaleString(i18n.language)
                    : (data.total_records ?? 0) > 0
                      ? `≥${data.total_records!.toLocaleString(i18n.language)}`
                      : "—"}
                </dd>
              </div>
              <div className="border-s border-text/[0.07] px-5 py-4">
                <dt className="pf-label">{t("statistics.totalAudio")}</dt>
                <dd className="mt-1 text-[24px] leading-none font-semibold tracking-[-0.03em] tabular-nums">
                  {formatAudioDuration(data.total_audio_ms)}
                </dd>
              </div>
            </dl>
          </section>
          <div
            data-statistics-cards
            className="grid grid-cols-2 gap-4 text-text"
          >
            <MonoActivityHeatmap
              stats={data}
              today={today}
              years={years}
              year={
                years.includes(heatmapYear)
                  ? heatmapYear
                  : years[years.length - 1]
              }
              setYear={setHeatmapYear}
              windowIndex={windowIndex}
              setWindowIndex={setWindowIndex}
            />
            <MonoRoundedLineChart
              stats={data}
              today={today}
              points={points}
              metric={metric}
              setMetric={setMetric}
              period={period}
              setPeriod={setPeriod}
              month={selectedMonth}
              setMonth={setMonth}
              firstMonth={firstMonth}
              lastMonth={lastMonth}
              year={selectedYear}
              setYear={setYear}
              years={years}
            />
          </div>
          <p className="px-1 text-[11.5px] leading-relaxed text-text-muted/80">
            {t("statistics.trackingSince", {
              date: formatPointDate(
                data.tracking_started_day,
                "day",
                i18n.language,
              ),
            })}
          </p>
        </>
      )}
    </div>
  );
}
