package computer.mybelovedrubble;

import android.app.Activity;
import android.content.ClipData;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.provider.OpenableColumns;
import android.util.Log;
import android.widget.Toast;
import java.io.File;
import java.io.FileNotFoundException;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayList;
import java.util.List;

/**
 * Brings files into the app: those another app shares with it (SEND and
 * SEND_MULTIPLE), and those picked with the system's picker when the app
 * asks (PICK, from src/gui.rs). An invisible activity, since only an
 * activity gets the picker's result and the native one can't.
 *
 * Each file is copied into the inbox, filesDir/inbox/<id>/<its name>, where
 * the app takes it into the open folder (src/gui.rs). It is copied into
 * filesDir/inbox/.<id> first, which the app skips.
 */
public class Inbox extends Activity {
    static final String PICK = "computer.mybelovedrubble.PICK";

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        // Back after being recreated: the picker's result is on its way.
        if (state != null) {
            return;
        }
        Intent intent = getIntent();
        String action = intent.getAction();
        List<Uri> uris = new ArrayList<>();
        if (PICK.equals(action)) {
            Intent pick = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            pick.addCategory(Intent.CATEGORY_OPENABLE);
            pick.setType("*/*");
            pick.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
            startActivityForResult(pick, 0);
            return;
        } else if (Intent.ACTION_SEND.equals(action)) {
            Uri uri = intent.getParcelableExtra(Intent.EXTRA_STREAM);
            if (uri != null) {
                uris.add(uri);
            }
        } else if (Intent.ACTION_SEND_MULTIPLE.equals(action)) {
            List<Uri> shared = intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM);
            if (shared != null) {
                uris.addAll(shared);
            }
        }
        take(uris, true);
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        List<Uri> uris = new ArrayList<>();
        if (result == RESULT_OK && data != null) {
            ClipData clip = data.getClipData();
            if (clip != null) {
                for (int i = 0; i < clip.getItemCount(); i++) {
                    uris.add(clip.getItemAt(i).getUri());
                }
            } else if (data.getData() != null) {
                uris.add(data.getData());
            }
        }
        take(uris, false);
    }

    /**
     * Copy `uris` into the inbox, off the main thread, then go: to the
     * app, for files another app `shared`.
     */
    private void take(List<Uri> uris, boolean shared) {
        if (uris.isEmpty()) {
            if (shared) {
                Toast.makeText(this, "Only files can be added", Toast.LENGTH_LONG).show();
            }
            finish();
            return;
        }
        new Thread(() -> {
            int failed = 0;
            for (Uri uri : uris) {
                try {
                    copy(uri);
                } catch (IOException | RuntimeException e) {
                    Log.w("mbr", "cannot add " + uri, e);
                    failed++;
                }
            }
            int failures = failed;
            runOnUiThread(() -> {
                if (failures > 0) {
                    Toast.makeText(this, failures + " file(s) could not be added", Toast.LENGTH_LONG).show();
                }
                if (shared) {
                    // Back to the app's activity, not a second one on top of
                    // it: shared from the app itself, this one is in its task.
                    Intent app = getPackageManager().getLaunchIntentForPackage(getPackageName());
                    if (app != null) {
                        startActivity(app.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK
                            | Intent.FLAG_ACTIVITY_CLEAR_TOP | Intent.FLAG_ACTIVITY_SINGLE_TOP));
                    }
                }
                finish();
            });
        }).start();
    }

    private void copy(Uri uri) throws IOException {
        File inbox = new File(getFilesDir(), "inbox");
        String id = Long.toString(System.nanoTime());
        File partial = new File(inbox, "." + id);
        File done = new File(inbox, id);
        if (!partial.mkdirs() && !partial.isDirectory()) {
            throw new IOException("cannot create " + partial);
        }
        File file = new File(partial, name(uri));
        try (InputStream in = getContentResolver().openInputStream(uri);
             OutputStream out = new FileOutputStream(file)) {
            if (in == null) {
                throw new FileNotFoundException(uri.toString());
            }
            byte[] buf = new byte[64 * 1024];
            for (int n; (n = in.read(buf)) > 0; ) {
                out.write(buf, 0, n);
            }
        } catch (IOException | RuntimeException e) {
            file.delete();
            partial.delete();
            throw e;
        }
        if (!partial.renameTo(done)) {
            file.delete();
            partial.delete();
            throw new IOException("cannot move " + partial + " to " + done);
        }
    }

    /** The name `uri`'s file goes by, as its app says. */
    private String name(Uri uri) {
        String name = null;
        try (Cursor cursor = getContentResolver().query(
                uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (cursor != null && cursor.moveToFirst() && !cursor.isNull(0)) {
                name = cursor.getString(0);
            }
        } catch (RuntimeException e) {
            // Not a provider that answers: the uri's own name will do.
        }
        if (name == null || name.isEmpty()) {
            name = uri.getLastPathSegment();
        }
        if (name == null || name.isEmpty()) {
            name = "shared file";
        }
        name = name.replace('/', '_');
        return name.startsWith(".") ? "_" + name : name;
    }
}
