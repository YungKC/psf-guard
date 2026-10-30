import { useCallback, useEffect, useState } from 'react';
import { apiClient } from '../api/client';
import type { MosaicTargetDetail, ProjectMosaic, UserMosaic } from '../api/types';

interface ManualMosaicPickerProps {
  dbId: string;
  projectId: number;
  /** The user mosaic the next mosaic build uses, or `null` for the default mosaic. */
  selectedMosaicId: number | null;
  onSelect: (mosaicId: number | null) => void;
}

function formatDegrees(value: number | null): string {
  return value === null ? '—' : `${value.toFixed(2)}°`;
}

function targetDetail(target: MosaicTargetDetail): string {
  const parts: string[] = [];
  if (target.telescope || target.camera) {
    parts.push([target.telescope, target.camera].filter(Boolean).join(' · '));
  }
  if (target.width_px !== null && target.height_px !== null) {
    parts.push(`${target.width_px}×${target.height_px}`);
  }
  if (target.fov_width_degrees !== null && target.fov_height_degrees !== null) {
    parts.push(`${formatDegrees(target.fov_width_degrees)} × ${formatDegrees(target.fov_height_degrees)}`);
  }
  const filters = target.filters
    .map((filter) => `${filter.filter_name} ${filter.frames} (${filter.accepted} accepted)`)
    .join(', ');
  if (filters) parts.push(filters);
  return parts.join(' · ');
}

/** Pick the targets of one project for a mosaic, and choose which
 *  mosaic the next mosaic build uses. A mosaic is a named selection
 *  of targets, not a copy of their frames: grading and calibration
 *  stay per panel, and deleting a mosaic leaves every frame
 *  untouched. Sparse grids are allowed, so a mosaic need not fill
 *  a rectangle. */
export default function ManualMosaicPicker({
  dbId,
  projectId,
  selectedMosaicId,
  onSelect,
}: ManualMosaicPickerProps) {
  const [targets, setTargets] = useState<MosaicTargetDetail[]>([]);
  const [mosaics, setMosaics] = useState<UserMosaic[]>([]);
  const [defaultMosaic, setDefaultMosaic] = useState<ProjectMosaic | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [name, setName] = useState('');
  const [checked, setChecked] = useState<Set<number>>(new Set());
  const [saving, setSaving] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [loadedTargets, loadedMosaics, loadedDefault] = await Promise.all([
        apiClient.getMosaicTargets(dbId, projectId),
        apiClient.getUserMosaics(dbId, projectId),
        apiClient.getProjectMosaic(dbId, projectId),
      ]);
      setTargets(loadedTargets);
      setMosaics(loadedMosaics);
      setDefaultMosaic(loadedDefault);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : 'Failed to load mosaic targets');
    } finally {
      setLoading(false);
    }
  }, [dbId, projectId]);

  useEffect(() => {
    void load();
  }, [load]);

  const toggle = (targetId: number) => {
    setChecked((current) => {
      const next = new Set(current);
      if (next.has(targetId)) next.delete(targetId);
      else next.add(targetId);
      return next;
    });
  };

  const useDefaultAsSelection = () => {
    if (!defaultMosaic) return;
    setChecked(new Set(defaultMosaic.panels.map((panel) => panel.target_id)));
  };

  const create = async () => {
    if (name.trim() === '' || checked.size < 2) return;
    setSaving(true);
    setError(null);
    try {
      const created = await apiClient.createUserMosaic(dbId, projectId, {
        name: name.trim(),
        panels: [...checked].map((target_id) => ({ target_id })),
      });
      setMosaics((current) => [...current, created]);
      setName('');
      setChecked(new Set());
      onSelect(created.id);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : 'Failed to create mosaic');
    } finally {
      setSaving(false);
    }
  };

  const remove = async (mosaicId: number) => {
    setError(null);
    try {
      await apiClient.deleteUserMosaic(dbId, projectId, mosaicId);
      setMosaics((current) => current.filter((mosaic) => mosaic.id !== mosaicId));
      if (selectedMosaicId === mosaicId) onSelect(null);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : 'Failed to delete mosaic');
    }
  };

  if (loading) return <p>Loading mosaics…</p>;

  return (
    <section aria-label="Mosaics">
      <h3>Mosaics</h3>
      <p>
        A mosaic is a named selection of this project&apos;s targets. Builds
        use the selected mosaic&apos;s panels; the default mosaic is used
        when none is selected.
      </p>
      {error && <p role="alert">{error}</p>}
      <fieldset>
        <legend>Build with</legend>
        <label>
          <input
            type="radio"
            name="manual-mosaic"
            checked={selectedMosaicId === null}
            onChange={() => onSelect(null)}
          />
          Default mosaic
          {defaultMosaic
            ? ` (${defaultMosaic.source}, ${defaultMosaic.panels.length} panels)`
            : ' (none)'}
        </label>
        {mosaics.map((mosaic) => (
          <div key={mosaic.id}>
            <label>
              <input
                type="radio"
                name="manual-mosaic"
                checked={selectedMosaicId === mosaic.id}
                onChange={() => onSelect(mosaic.id)}
              />
              {mosaic.name} ({mosaic.panels.length} panels, {mosaic.rows}×{mosaic.columns})
            </label>
            <button type="button" onClick={() => void remove(mosaic.id)}>
              Delete {mosaic.name}
            </button>
          </div>
        ))}
      </fieldset>
      <fieldset>
        <legend>New mosaic</legend>
        <label>
          Name
          <input
            type="text"
            value={name}
            onChange={(event) => setName(event.target.value)}
            aria-label="Mosaic name"
          />
        </label>
        {defaultMosaic && (
          <button type="button" onClick={useDefaultAsSelection}>
            Start from the default mosaic ({defaultMosaic.panels.length} panels)
          </button>
        )}
        {targets.map((target) => (
          <label key={target.target_id}>
            <input
              type="checkbox"
              checked={checked.has(target.target_id)}
              onChange={() => toggle(target.target_id)}
            />
            {target.name} — {targetDetail(target)}
          </label>
        ))}
        {checked.size < 2 && <p>A mosaic needs at least two targets.</p>}
        <button type="button" disabled={name.trim() === '' || checked.size < 2 || saving} onClick={() => void create()}>
          Create mosaic
        </button>
      </fieldset>
      {mosaics.length > 0 && (
        <div>
          <h4>Grid preview</h4>
          {mosaics.map((mosaic) => (
            <div key={mosaic.id}>
              <p>{mosaic.name}</p>
              <table>
                <tbody>
                  {Array.from({ length: mosaic.rows }, (_, rowIndex) => (
                    <tr key={rowIndex}>
                      {Array.from({ length: mosaic.columns }, (_, columnIndex) => {
                        const panel = mosaic.panels.find(
                          (candidate) => candidate.row === rowIndex + 1 && candidate.column === columnIndex + 1
                        );
                        return <td key={columnIndex}>{panel ? panel.target_name : ''}</td>;
                      })}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}
