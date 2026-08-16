import type { Diagnosis } from "../types";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";

export function DiagnosisView({
  diagnosis,
  selectedEvidenceId,
  onSelectEvidence,
  onProposeRemediation,
  proposing,
}: {
  diagnosis: Diagnosis;
  selectedEvidenceId?: string | null;
  onSelectEvidence?: (evidenceId: string) => void;
  onProposeRemediation?: (claimIds: string[]) => void;
  proposing?: boolean;
}) {
  const claimIds = diagnosis.claims
    .map((claim) => claim.claim_id)
    .filter((id): id is string => typeof id === "string" && id.length > 0);

  return (
    <section className="mt-3 space-y-3 rounded-md border border-zinc-200 bg-white p-4 shadow-sm">
      <div>
        <h3 className="text-[10px] font-semibold uppercase tracking-wide text-zinc-500">Diagnosis</h3>
        <p className="mt-1 text-sm leading-6 text-zinc-800">{diagnosis.summary}</p>
      </div>
      {diagnosis.claims.length > 0 && (
        <ul className="space-y-2">
          {diagnosis.claims.map((claim, index) => (
            <li key={claim.claim_id ?? `${claim.kind}-${index}`} className="rounded-md bg-zinc-50 px-3 py-2">
              <div className="mb-1 flex flex-wrap items-center gap-2">
                <Badge className="bg-zinc-100 text-zinc-700 ring-zinc-200">{claim.kind}</Badge>
                {claim.confidence && (
                  <Badge className="bg-sky-50 text-sky-700 ring-sky-200">{claim.confidence}</Badge>
                )}
              </div>
              <p className="text-sm text-zinc-800">{claim.statement}</p>
              {claim.evidence_ids && claim.evidence_ids.length > 0 && (
                <div className="mt-2 flex flex-wrap gap-1">
                  {claim.evidence_ids.map((evidenceId) => (
                    <button
                      key={evidenceId}
                      type="button"
                      aria-label={`Evidence ${evidenceId}`}
                      className={
                        selectedEvidenceId === evidenceId
                          ? "rounded bg-emerald-100 px-2 py-0.5 font-mono text-[10px] text-emerald-800"
                          : "rounded bg-white px-2 py-0.5 font-mono text-[10px] text-zinc-600 ring-1 ring-zinc-200"
                      }
                      onClick={() => onSelectEvidence?.(evidenceId)}
                    >
                      {evidenceId.length > 12 ? `${evidenceId.slice(0, 8)}…` : evidenceId}
                    </button>
                  ))}
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
      {diagnosis.limitations && diagnosis.limitations.length > 0 && (
        <div>
          <h4 className="text-[10px] font-semibold uppercase text-amber-700">Limitations</h4>
          <ul className="mt-1 list-disc space-y-1 pl-4 text-xs text-amber-800">
            {diagnosis.limitations.map((item) => (
              <li key={item}>{item}</li>
            ))}
          </ul>
        </div>
      )}
      {diagnosis.recommended_actions && diagnosis.recommended_actions.length > 0 && (
        <div>
          <h4 className="text-[10px] font-semibold uppercase text-zinc-500">Recommended actions</h4>
          <ul className="mt-1 list-disc space-y-1 pl-4 text-xs text-zinc-700">
            {diagnosis.recommended_actions.map((item) => (
              <li key={item}>{item}</li>
            ))}
          </ul>
        </div>
      )}
      {onProposeRemediation && (
        <div className="flex justify-end border-t border-zinc-100 pt-3">
          <Button
            size="sm"
            variant="outline"
            disabled={proposing}
            onClick={() => onProposeRemediation(claimIds)}
          >
            Propose remediation
          </Button>
        </div>
      )}
    </section>
  );
}
