// Props come from the serving binary, so an older browser bundle cannot invent its version.
interface BuildInfo {
  revision: string;
  commit: string;
  commitUrl: string;
  builtAt: string;
}

export default function BuildVersion({ revision, commit, commitUrl, builtAt }: BuildInfo) {
  const date = new Date(Number(builtAt) * 1000);
  const local = date.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
  const tooltip = `${commit}\nBuilt ${date.toISOString()}`;
  return <span className="build-version" title={tooltip}>
    {commitUrl ? <a href={commitUrl} target="_blank" rel="noreferrer">{revision}</a> : revision}
    {" · "}<time dateTime={date.toISOString()}>built {local}</time>
  </span>;
}
