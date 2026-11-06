#include "stdafx.h"

#include "CListCtrl_Sortable.h"
#include "Resource.h"
#include "mainDlg.h"

#include <uxtheme.h>

BEGIN_MESSAGE_MAP(CListCtrl_Sortable, CListCtrl_LabelTip)
    ON_NOTIFY_REFLECT_EX(LVN_COLUMNCLICK, OnHeaderClick)	// Column Click
END_MESSAGE_MAP()

namespace {
    bool IsThemeEnabled()
    {
        return IsAppThemed() && IsThemeActive();
    }
}

BOOL CListCtrl_Sortable::OnHeaderClick(NMHDR* pNMHDR, LRESULT* pResult)
{
    NMLISTVIEW* pLV = reinterpret_cast<NMLISTVIEW*>(pNMHDR);
    SetFocus();	// Ensure other controls gets kill-focus

    int colIndex = pLV->iSubItem;

    if (m_SortCol == colIndex)
    {
        m_Ascending = !m_Ascending;
    }
    else
    {
        m_SortCol = colIndex;
        m_Ascending = true;
    }

    if (SortColumn(m_SortCol, m_Ascending))
        SetSortArrow(m_SortCol, m_Ascending);

    return FALSE;	// Let parent-dialog get chance
}

void CListCtrl_Sortable::SetSortArrow(int colIndex, bool ascending)
{
    if (IsThemeEnabled())
    {
        for (int i = 0; i < GetHeaderCtrl()->GetItemCount(); ++i)
        {
            HDITEM hditem = { 0 };
            hditem.mask = HDI_FORMAT;
            VERIFY(GetHeaderCtrl()->GetItem(i, &hditem));
            hditem.fmt &= ~(HDF_SORTDOWN | HDF_SORTUP);
            if (i == colIndex)
            {
                hditem.fmt |= ascending ? HDF_SORTUP : HDF_SORTDOWN;
            }
            VERIFY(CListCtrl_LabelTip::GetHeaderCtrl()->SetItem(i, &hditem));
        }
    }
    else
    {
        UINT bitmapID = m_Ascending ? IDB_UPARROW : IDB_DOWNARROW;
        for (int i = 0; i < GetHeaderCtrl()->GetItemCount(); ++i)
        {
            HDITEM hditem = { 0 };
            hditem.mask = HDI_BITMAP | HDI_FORMAT;
            VERIFY(GetHeaderCtrl()->GetItem(i, &hditem));
            if (hditem.fmt & HDF_BITMAP && hditem.fmt & HDF_BITMAP_ON_RIGHT)
            {
                if (hditem.hbm)
                {
                    DeleteObject(hditem.hbm);
                    hditem.hbm = NULL;
                }
                hditem.fmt &= ~(HDF_BITMAP | HDF_BITMAP_ON_RIGHT);
                VERIFY(CListCtrl_LabelTip::GetHeaderCtrl()->SetItem(i, &hditem));
            }
            if (i == colIndex)
            {
                hditem.fmt |= HDF_BITMAP | HDF_BITMAP_ON_RIGHT;
                hditem.hbm = (HBITMAP)LoadImage(GetModuleHandle(NULL), MAKEINTRESOURCE(bitmapID), IMAGE_BITMAP, 0, 0, LR_LOADMAP3DCOLORS);
                VERIFY(hditem.hbm != NULL);
                VERIFY(CListCtrl_LabelTip::GetHeaderCtrl()->SetItem(i, &hditem));
            }
        }
    }
}

void CListCtrl_Sortable::PreSubclassWindow()
{
    CListCtrl_LabelTip::PreSubclassWindow();

    // Focus retangle is not painted properly without double-buffering
    SetExtendedStyle(LVS_EX_DOUBLEBUFFER | GetExtendedStyle());
    SetExtendedStyle(GetExtendedStyle() | LVS_EX_FULLROWSELECT);
    SetExtendedStyle(GetExtendedStyle() | LVS_EX_HEADERDRAGDROP);
    
    SetWindowTheme(GetSafeHwnd(), L"Explorer", nullptr);
}

void CListCtrl_Sortable::ResetSortOrder()
{
    m_Ascending = true;
    m_SortCol = -1;
    SetSortArrow(m_SortCol, m_Ascending);
}

// The column version of GetItemData(), one can specify an unique
// identifier when using InsertColumn()
int CListCtrl_Sortable::GetColumnData(int col) const
{
    LVCOLUMN lvc = { 0 };
    lvc.mask = LVCF_SUBITEM;
    VERIFY(GetColumn(col, &lvc));
    return lvc.iSubItem;
}

void CListCtrl_Sortable::SetSortColumn(int columnIndex, bool ascending)
{
    m_SortCol = columnIndex;
    m_Ascending = ascending;
    if (SortColumn(m_SortCol, m_Ascending)) {
        SetSortArrow(m_SortCol, m_Ascending);
    }
}
