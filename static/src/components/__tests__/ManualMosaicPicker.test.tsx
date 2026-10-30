import { describe, expect, it } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { http, HttpResponse } from 'msw';
import { server } from '../../test/msw-server';
import ManualMosaicPicker from '../ManualMosaicPicker';

const ok = (data: unknown) => HttpResponse.json({ success: true, data, error: null, status: 'ready' });

const target = (id: number, name: string) => ({
  target_id: id,
  name,
  ra_hours: 22.3,
  dec_degrees: 55.8,
  filters: [{ filter_name: 'Ha', frames: 10, accepted: 10 }],
  telescope: 'SCA260 V2',
  camera: 'ZWO ASI6200MM Pro',
  width_px: 9576,
  height_px: 6388,
  fov_width_degrees: 1.6,
  fov_height_degrees: 1.07,
});

describe('ManualMosaicPicker', () => {
  it('lists targets with rig and filter detail, creates a mosaic, and selects it', async () => {
    let created: unknown = null;
    server.use(
      http.get('/api/db/test/projects/1/mosaic-targets', () => ok([
        target(1, 'LDN 1163 Panel 1'),
        target(2, 'LDN 1163 Panel 2'),
        target(3, 'LDN 1163 Panel 0'),
      ])),
      http.get('/api/db/test/projects/1/mosaics', () => ok([])),
      http.get('/api/db/test/projects/1/mosaic', () => ok({ mosaic: null })),
      http.post('/api/db/test/projects/1/mosaics', async ({ request }) => {
        created = await request.json();
        return ok({
          id: 7,
          project_id: 1,
          name: 'Mono 3-panel',
          source: 'user',
          rows: 2,
          columns: 2,
          panels: [
            { target_id: 3, target_name: 'LDN 1163 Panel 0', panel_id: 'r1c2', row: 1, column: 2 },
            { target_id: 1, target_name: 'LDN 1163 Panel 1', panel_id: 'r2c1', row: 2, column: 1 },
            { target_id: 2, target_name: 'LDN 1163 Panel 2', panel_id: 'r2c2', row: 2, column: 2 },
          ],
        });
      }),
    );
    let selected: number | null = null;
    render(
      <ManualMosaicPicker
        dbId="test"
        projectId={1}
        selectedMosaicId={null}
        onSelect={(id) => { selected = id; }}
      />
    );
    await waitFor(() => expect(screen.getByText(/LDN 1163 Panel 0/)).toBeInTheDocument());
    expect(screen.getAllByText(/SCA260 V2/)).toHaveLength(3);
    expect(screen.getAllByText(/Ha 10 \(10 accepted\)/)).toHaveLength(3);

    const createButton = screen.getByRole('button', { name: 'Create mosaic' });
    expect(createButton).toBeDisabled();
    fireEvent.change(screen.getByLabelText('Mosaic name'), { target: { value: 'Mono 3-panel' } });
    fireEvent.click(screen.getByText(/LDN 1163 Panel 1 —/));
    fireEvent.click(screen.getByText(/LDN 1163 Panel 2 —/));
    expect(createButton).not.toBeDisabled();
    fireEvent.click(createButton);
    await waitFor(() => expect(selected).toBe(7));
    expect(created).toEqual({
      name: 'Mono 3-panel',
      panels: [{ target_id: 1 }, { target_id: 2 }],
    });
  });

  it('does not offer a one-target mosaic', async () => {
    server.use(
      http.get('/api/db/test/projects/1/mosaic-targets', () => ok([target(1, 'LDN 1163 Panel 1')])),
      http.get('/api/db/test/projects/1/mosaics', () => ok([])),
      http.get('/api/db/test/projects/1/mosaic', () => ok({ mosaic: null })),
    );
    render(
      <ManualMosaicPicker dbId="test" projectId={1} selectedMosaicId={null} onSelect={() => {}} />
    );
    await waitFor(() => expect(screen.getByText(/LDN 1163 Panel 1/)).toBeInTheDocument());
    expect(screen.getByText('A mosaic needs at least two targets.')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Create mosaic' })).toBeDisabled();
  });
});
