package computer.mybelovedrubble;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.OpenableColumns;
import android.webkit.MimeTypeMap;
import java.io.File;
import java.io.FileNotFoundException;
import java.io.IOException;

/**
 * Lends other apps read access to the files in the app's own storage,
 * for "Open with" and "Share": content://computer.mybelovedrubble.files/<path
 * under the files dir>, readable only through a grant on an intent (the
 * provider isn't exported). The app is otherwise native (src/gui.rs).
 */
public class Files extends ContentProvider {
    @Override
    public boolean onCreate() {
        return true;
    }

    /** The file `uri` names, if it is (or links to a file) in the app's own storage. */
    private File file(Uri uri) throws FileNotFoundException {
        String path = uri.getPath();
        if (path == null) {
            throw new FileNotFoundException(uri.toString());
        }
        try {
            File base = getContext().getFilesDir().getCanonicalFile();
            File file = new File(base, path).getCanonicalFile();
            if (!file.getPath().startsWith(base.getPath() + "/") || !file.isFile()) {
                throw new FileNotFoundException(uri.toString());
            }
            return file;
        } catch (IOException e) {
            throw new FileNotFoundException(e.getMessage());
        }
    }

    @Override
    public ParcelFileDescriptor openFile(Uri uri, String mode) throws FileNotFoundException {
        if (!"r".equals(mode)) {
            throw new SecurityException("read only");
        }
        return ParcelFileDescriptor.open(file(uri), ParcelFileDescriptor.MODE_READ_ONLY);
    }

    // The name comes from the uri: the file itself is a blob named by its hash.
    @Override
    public String getType(Uri uri) {
        String name = uri.getLastPathSegment();
        int dot = name == null ? -1 : name.lastIndexOf('.');
        String type = dot < 0 ? null
            : MimeTypeMap.getSingleton().getMimeTypeFromExtension(name.substring(dot + 1).toLowerCase());
        return type != null ? type : "application/octet-stream";
    }

    @Override
    public Cursor query(Uri uri, String[] projection, String selection, String[] selectionArgs, String sortOrder) {
        File file;
        try {
            file = file(uri);
        } catch (FileNotFoundException e) {
            return null;
        }
        if (projection == null) {
            projection = new String[] {OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE};
        }
        MatrixCursor cursor = new MatrixCursor(projection, 1);
        Object[] row = new Object[projection.length];
        for (int i = 0; i < projection.length; i++) {
            if (OpenableColumns.DISPLAY_NAME.equals(projection[i])) {
                row[i] = uri.getLastPathSegment();
            } else if (OpenableColumns.SIZE.equals(projection[i])) {
                row[i] = file.length();
            }
        }
        cursor.addRow(row);
        return cursor;
    }

    @Override
    public Uri insert(Uri uri, ContentValues values) {
        throw new UnsupportedOperationException("read only");
    }

    @Override
    public int update(Uri uri, ContentValues values, String selection, String[] selectionArgs) {
        throw new UnsupportedOperationException("read only");
    }

    @Override
    public int delete(Uri uri, String selection, String[] selectionArgs) {
        throw new UnsupportedOperationException("read only");
    }
}
